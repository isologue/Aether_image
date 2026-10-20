#!/bin/sh
set -eu

# Collect Node and native Sharp dependencies for the existing shell-free images.
root=/runtime-root
mkdir -p "$root/usr/local/bin" "$root/opt/aether/image-resizer"
cp /usr/local/bin/node "$root/usr/local/bin/node"
cp -R /opt/aether/image-resizer/node_modules /opt/aether/image-resizer/resize.mjs "$root/opt/aether/image-resizer/"
{ printf '%s\n' /usr/local/bin/node; find /opt/aether/image-resizer/node_modules -type f \( -name '*.node' -o -name '*.so*' \); } > /tmp/sharp-native-files
while IFS= read -r native; do
    ldd "$native" | awk '/=> \// { print $3 } $1 ~ /^\// { print $1 }'
done < /tmp/sharp-native-files | sort -u > /tmp/sharp-runtime-libs
while IFS= read -r lib; do
    cp -L --parents "$lib" "$root/"
done < /tmp/sharp-runtime-libs

# Check native module loading and processing without relying on the builder's filesystem.
chroot "$root" /usr/local/bin/node --input-type=module -e '
  import { resizeImage } from "/opt/aether/image-resizer/resize.mjs";
  import sharp from "/opt/aether/image-resizer/node_modules/sharp/lib/index.js";
  const input = await sharp({ create: { width: 2, height: 2, channels: 4, background: "red" } }).png().toBuffer();
  const output = await resizeImage(input, 4, 4);
  const metadata = await sharp(output).metadata();
  if (metadata.width !== 4 || metadata.height !== 4) throw new Error("Sharp runtime smoke test failed");
'
