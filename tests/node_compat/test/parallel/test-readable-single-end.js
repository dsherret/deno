// deno-fmt-ignore-file
// deno-lint-ignore-file

// Copyright Joyent and Node contributors. All rights reserved. MIT license.
// Taken from Node 23.9.0
// This file is automatically generated by `tests/node_compat/runner/setup.ts`. Do not modify this file manually.

'use strict';

const common = require('../common');
const { Readable } = require('stream');

// This test ensures that there will not be an additional empty 'readable'
// event when stream has ended (only 1 event signalling about end)

const r = new Readable({
  read: () => {},
});

r.push(null);

r.on('readable', common.mustCall());
r.on('end', common.mustCall());
