import { parse } from 'node:path';

import test from 'ava';
import alias from 'macos-alias';

import { create } from '../index.js';
import { fileURLToPath } from 'node:url';

const selfpath = fileURLToPath(import.meta.url);

test('create should work', (t) => {
  const buf0 = alias.create(selfpath);
  const buf1 = create(selfpath);
  const info0 = alias.decode(buf0);
  const info1 = alias.decode(buf1);

  t.is('file', info1.target.type);
  t.is(parse(selfpath).base, info1.target.filename);

  t.deepEqual(info0.extra, info1.extra);
});
