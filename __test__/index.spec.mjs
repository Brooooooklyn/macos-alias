import { join, parse } from 'node:path'

import test from 'ava'
import alias from 'macos-alias'

import { create } from '../index.js'
import { fileURLToPath } from 'node:url'

const selfpath = fileURLToPath(import.meta.url)

test('create should work', (t) => {
  const buf = create(selfpath)
  const info = alias.decode(buf)

  t.is('file', info.target.type)
  t.is(parse(selfpath).base, info.target.filename)
})
