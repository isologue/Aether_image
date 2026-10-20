import sharp from 'sharp'
import { pathToFileURL } from 'node:url'

const MAX_BYTES = 64 * 1024 * 1024
const MAX_PIXELS = 32_000_000

// The gateway allows two concurrent processes; each uses one libvips worker.
sharp.concurrency(1)
sharp.cache(false)

export async function resizeImage(input, width, height) {
  if (!Number.isInteger(width) || !Number.isInteger(height) || width < 1 || height < 1 ||
      width > 8192 || height > 8192 || width * height > MAX_PIXELS) {
    throw new Error('Invalid target dimensions')
  }
  if (!input.length || input.length > MAX_BYTES) throw new Error('Invalid image length')
  const image = sharp(input, { limitInputPixels: MAX_PIXELS, failOn: 'error' })
  const metadata = await image.metadata()
  if (!['png', 'jpeg', 'webp'].includes(metadata.format) || (metadata.pages ?? 1) > 1) {
    throw new Error('Only still PNG, JPEG and WebP images are supported')
  }
  // Preserve exact bytes when no resizing or orientation correction is needed.
  if (metadata.width === width && metadata.height === height && (!metadata.orientation || metadata.orientation === 1)) {
    return input
  }
  // Preserve the whole image. Mismatched aspect ratios get transparent padding
  // (white for JPEG), rather than stretching the image or cropping its content.
  let pipeline = image.rotate().resize(width, height, {
    kernel: sharp.kernel.lanczos3,
    fit: 'contain',
    background: { r: 255, g: 255, b: 255, alpha: metadata.format === 'jpeg' ? 1 : 0 },
  })
  if (metadata.format === 'png') pipeline = pipeline.png()
  if (metadata.format === 'jpeg') pipeline = pipeline.jpeg({ quality: 95, chromaSubsampling: '4:4:4' })
  if (metadata.format === 'webp') pipeline = pipeline.webp({ lossless: true })
  const result = await pipeline.toBuffer()
  if (result.length > MAX_BYTES) throw new Error('Resized image exceeds 64 MiB')
  return result
}

async function main() {
  let length = 0
  const chunks = []
  for await (const chunk of process.stdin) {
    length += chunk.length
    if (length > MAX_BYTES) throw new Error('Input image exceeds 64 MiB')
    chunks.push(chunk)
  }
  const result = await resizeImage(Buffer.concat(chunks), Number(process.argv[2]), Number(process.argv[3]))
  process.stdout.write(result)
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch(error => {
    console.error(error.message)
    process.exitCode = 1
  })
}
