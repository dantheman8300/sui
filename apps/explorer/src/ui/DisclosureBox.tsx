// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

import { Disclosure } from '@headlessui/react';

import type { ReactNode } from 'react';

import { ReactComponent as ChevronDownIcon } from '~/assets/SVGIcons/16px/chevron-down.svg';

export type DisclosureBoxProps = {
    defaultOpen?: boolean;
    title: ReactNode;
    children: ReactNode | ReactNode[];
};

export function DisclosureBox({
    defaultOpen,
    title,
    children,
}: DisclosureBoxProps) {
    return (
        <div className="rounded-lg bg-sui-grey-40">
            <Disclosure defaultOpen={defaultOpen}>
                <Disclosure.Button
                    as="div"
                    className="flex flex-nowrap cursor-pointer items-center pt-3.5 pb-4 px-5"
                >
                    <div className="flex-1 text-body font-semibold text-sui-grey-90">
                        {title}
                    </div>
                    <ChevronDownIcon className="-rotate-90 ui-open:rotate-0 text-sui-grey-75" />
                </Disclosure.Button>
                <Disclosure.Panel className="text-gray-500 px-5 pb-5">
                    {children}
                </Disclosure.Panel>
            </Disclosure>
        </div>
    );
}
