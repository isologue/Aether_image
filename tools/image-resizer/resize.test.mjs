import test from 'node:test'
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import sharp from 'sharp'
import { resizeImage } from './resize.mjs'

const fixture = format => sharp({ create: {
  width: 12, height: 8, channels: 4, background: { r: 200, g: 20, b: 10, alpha: 1 },
} }).toFormat(format).toBuffer()

for (const format of ['png', 'jpeg', 'webp']) {
  test(`${format}: scales up/down to exact dimensions and retains format`, async () => {
    const input = await fixture(format)
    for (const [width, height] of [[48, 32], [6, 4], [32, 32]]) {
      const result = await resizeImage(input, width, height)
      const metadata = await sharp(result).metadata()
      assert.equal(metadata.width, width)
      assert.equal(metadata.height, height)
      assert.equal(metadata.format, format)
    }
    assert.deepEqual(await resizeImage(input, 12, 8), input)
  })
}

test('aspect ratio changes pad transparently without stretching or cropping', async () => {
  const result = await resizeImage(await fixture('png'), 24, 24)
  const { data, info } = await sharp(result).raw().toBuffer({ resolveWithObject: true })
  assert.equal(info.channels, 4)
  assert.equal(data[3], 0)
  const center = (12 * info.width + 12) * info.channels
  assert.equal(data[center], 200)
  assert.equal(data[center + 3], 255)
})

test('invalid images and oversized dimensions fail instead of returning unscaled data', async () => {
  const input = await fixture('png')
  for (const [width, height] of [[0, 10], [10, -1], [8193, 10], [8192, 8192], [1.5, 2]]) {
    await assert.rejects(resizeImage(input, width, height), /Invalid target dimensions/)
  }
  await assert.rejects(resizeImage(Buffer.from('not an image'), 20, 20))
  await assert.rejects(resizeImage(await fixture('gif'), 20, 20), /Only still/)
})

test('CLI accepts binary stdin and emits only the resized image on stdout', async () => {
  const input = await fixture('png')
  const child = spawn(process.execPath, [fileURLToPath(new URL('./resize.mjs', import.meta.url)), '48', '32'])
  const output = []
  child.stdout.on('data', chunk => output.push(chunk))
  child.stdin.end(input)
  const code = await new Promise((resolve, reject) => {
    child.on('error', reject)
    child.on('close', resolve)
  })
  assert.equal(code, 0)
  const metadata = await sharp(Buffer.concat(output)).metadata()
  assert.equal(metadata.width, 48)
  assert.equal(metadata.height, 32)
})
