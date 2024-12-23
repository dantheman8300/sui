// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use std::{collections::BTreeSet, sync::Arc};

use mysten_metrics::spawn_monitored_task;
use tokio::{
    sync::mpsc,
    task::JoinHandle,
    time::{interval, MissedTickBehavior},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::{
    db::Db,
    handlers::Handler,
    metrics::IndexerMetrics,
    models::watermarks::{CommitterWatermark, Ordering},
    pipeline::PipelineConfig,
};

/// Tracing message for the watermark update will be logged at info level at least this many
/// checkpoints.
const LOUD_WATERMARK_UPDATE_INTERVAL: i64 = 5 * 10;

/// Issue a warning every time the number of pending watermarks exceeds this number. This can
/// happen if the pipeline was started with its initial checkpoint overridden to be strictly
/// greater than its current watermark -- in that case, the pipeline will never be able to update
/// its watermarks.
///
/// This may be a legitimate thing to do when backfilling a table, but in that case
/// `--skip-watermarks` should be used.
const WARN_PENDING_WATERMARKS: usize = 10000;

/// The watermark task is responsible for keeping track of a pipeline's out-of-order commits and
/// updating its row in the `watermarks` table when a continuous run of checkpoints have landed
/// since the last watermark update.
///
/// It receives watermarks for each checkpoint that has been completely written out by the
/// committer, and periodically (on a configurable interval) checks if the watermark for the
/// pipeline can be pushed forward.
///
/// If it detects that more than [WARN_PENDING_WATERMARKS] watermarks have built up, it will issue
/// a warning, as this could be the indication of a memory leak, and the caller probably intended
/// to run the indexer with watermarking disabled (e.g. if they are running a backfill).
///
/// The task regularly traces its progress, outputting at a higher log level every
/// [LOUD_WATERMARK_UPDATE_INTERVAL]-many checkpoints.
///
/// The task will shutdown if the `cancel` token is signalled, or if the `rx` channel closes and
/// the watermark cannot be progressed. If the `config` specifies `skip_watermark`, the task will
/// shutdown immediately.
pub(super) fn watermark<H: Handler + 'static>(
    initial_watermark: Option<CommitterWatermark<'static>>,
    config: PipelineConfig,
    mut rx: mpsc::Receiver<Vec<CommitterWatermark<'static>>>,
    db: Db,
    metrics: Arc<IndexerMetrics>,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    spawn_monitored_task!(async move {
        if config.skip_watermark {
            info!(pipeline = H::NAME, "Skipping watermark task");
            return;
        }

        let mut poll = interval(config.watermark_interval);
        poll.set_missed_tick_behavior(MissedTickBehavior::Delay);

        // To correctly update the watermark, the committer tracks the watermark it last tried to
        // write and the watermarks for any checkpoints that have been written since then
        // ("pre-committed"). After each batch is written, the committer will try to progress the
        // watermark as much as possible without going over any holes in the sequence of
        // checkpoints.
        //
        // NOTE: When no watermark is provided, it is assumed that the pipeline is starting from
        // scratch, but we still initialize it as if it is at (after) the genesis checkpoint. This
        // means we never write a watermark for the genesis checkpoint, but would wait for another
        // checkpoint to be written out before updating the watermark, which is fine in practice
        // and simplifies the logic of tracking watermarks.
        let mut precommitted: BTreeSet<CommitterWatermark<'static>> = BTreeSet::new();
        let mut watermark =
            initial_watermark.unwrap_or_else(|| CommitterWatermark::initial(H::NAME.into()));

        // The committer will periodically output a log message at a higher log level to
        // demonstrate that the pipeline is making progress.
        let mut next_loud_watermark_update =
            watermark.checkpoint_hi_inclusive + LOUD_WATERMARK_UPDATE_INTERVAL;

        info!(pipeline = H::NAME, ?watermark, "Starting watermark task");
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    info!(pipeline = H::NAME, "Shutdown received, stopping watermark");
                    break;
                }

                _ = poll.tick() => {
                    if precommitted.len() > WARN_PENDING_WATERMARKS {
                        warn!(
                            pipeline = H::NAME,
                            pending = precommitted.len(),
                            "Pipeline has a large number of pending watermarks",
                        );
                    }

                    let Ok(mut conn) = db.connect().await else {
                        warn!(pipeline = H::NAME, "Committer failed to get connection for DB");
                        continue;
                    };

                    // Check if the pipeline's watermark needs to be updated
                    let guard = metrics
                        .watermark_gather_latency
                        .with_label_values(&[H::NAME])
                        .start_timer();

                    let mut watermark_needs_update = false;
                    while let Some(pending) = precommitted.first() {
                        match watermark.next_cmp(pending) {
                            Ordering::Future => break,
                            Ordering::Past => {
                                // Out of order watermarks can be encountered when a pipeline is
                                // starting up, because ingestion must start at the lowest
                                // checkpoint across all pipelines, or because of a backfill, where
                                // the initial checkpoint has been overridden.
                                //
                                // Track how many we see to make sure it doesn't grow without
                                // bound.
                                metrics
                                    .total_watermarks_out_of_order
                                    .with_label_values(&[H::NAME])
                                    .inc();

                                precommitted.pop_first().unwrap();
                            }

                            Ordering::Next => {
                                // SAFETY: `precommitted` is known to be non-empty because of the loop
                                // condition.
                                watermark = precommitted.pop_first().unwrap();
                                watermark_needs_update = true;
                            }
                        }
                    }

                    let elapsed = guard.stop_and_record();

                    metrics
                        .watermark_epoch
                        .with_label_values(&[H::NAME])
                        .set(watermark.epoch_hi_inclusive);

                    metrics
                        .watermark_checkpoint
                        .with_label_values(&[H::NAME])
                        .set(watermark.checkpoint_hi_inclusive);

                    metrics
                        .watermark_transaction
                        .with_label_values(&[H::NAME])
                        .set(watermark.tx_hi);

                    debug!(
                        pipeline = H::NAME,
                        elapsed_ms = elapsed * 1000.0,
                        watermark = watermark.checkpoint_hi_inclusive,
                        pending = precommitted.len(),
                        "Gathered watermarks",
                    );

                    if watermark_needs_update {
                        let guard = metrics
                            .watermark_commit_latency
                            .with_label_values(&[H::NAME])
                            .start_timer();

                        match watermark.update(&mut conn).await {
                            // If there's an issue updating the watermark, log it but keep going,
                            // it's OK for the watermark to lag from a correctness perspective.
                            Err(e) => {
                                let elapsed = guard.stop_and_record();
                                error!(
                                    pipeline = H::NAME,
                                    elapsed_ms = elapsed * 1000.0,
                                    ?watermark,
                                    "Error updating watermark: {e}",
                                );
                            }

                            Ok(updated) => {
                                let elapsed = guard.stop_and_record();

                                if updated {
                                    metrics
                                        .watermark_epoch_in_db
                                        .with_label_values(&[H::NAME])
                                        .set(watermark.epoch_hi_inclusive);

                                    metrics
                                        .watermark_checkpoint_in_db
                                        .with_label_values(&[H::NAME])
                                        .set(watermark.checkpoint_hi_inclusive);

                                    metrics
                                        .watermark_transaction_in_db
                                        .with_label_values(&[H::NAME])
                                        .set(watermark.tx_hi);
                                }

                                if watermark.checkpoint_hi_inclusive > next_loud_watermark_update {
                                    next_loud_watermark_update += LOUD_WATERMARK_UPDATE_INTERVAL;
                                    info!(
                                        pipeline = H::NAME,
                                        elapsed_ms = elapsed * 1000.0,
                                        updated,
                                        epoch = watermark.epoch_hi_inclusive,
                                        checkpoint = watermark.checkpoint_hi_inclusive,
                                        transaction = watermark.tx_hi,
                                        "Watermark",
                                    );
                                } else {
                                    debug!(
                                        pipeline = H::NAME,
                                        elapsed_ms = elapsed * 1000.0,
                                        updated,
                                        epoch = watermark.epoch_hi_inclusive,
                                        checkpoint = watermark.checkpoint_hi_inclusive,
                                        transaction = watermark.tx_hi,
                                        "Watermark",
                                    );
                                }
                            }
                        }
                    }

                    if rx.is_closed() {
                        info!(pipeline = H::NAME, "Committer closed channel, stopping watermark");
                        break;
                    }
                }

                Some(watermarks) = rx.recv() => {
                    precommitted.extend(watermarks);
                }
            }
        }
    })
}
