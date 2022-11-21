// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0
import { useQuery } from '@tanstack/react-query';

import { useRpc } from './useRpc';

import type { ObjectId } from '@mysten/sui.js';

export function useNormalizedMoveModule(
    packageId: ObjectId | null | undefined,
    moduleName: string | null | undefined
) {
    const rpc = useRpc();
    return useQuery(
        ['normalized-module', packageId, moduleName],
        async () => {
            return await rpc.getNormalizedMoveModule(packageId!, moduleName!);
        },
        {
            enabled: !!(packageId && moduleName),
        }
    );
}
