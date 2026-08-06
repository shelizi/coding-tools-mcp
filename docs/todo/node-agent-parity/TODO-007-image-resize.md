<!-- parity-id: NP-007 -->
<!-- parity-status: done -->
# NP-007 — Image identification and bounded resize

- Priority: P1
- Area: file-contract
- Status: done

## Gap

Resolved in Node Agent 0.16.0. `view_image` now identifies supported image formats from content, returns dimensions and original metadata, and performs bounded proportional PNG/JPEG resizing without native binaries.

## Rust evidence

- `src-tauri/src/tools/image_tool.rs`

## Node current state

- `packages/node-agent/src/imageCodec.ts`
- `packages/node-agent/src/fileTools.ts`
- `packages/node-agent/scripts/check-native.mjs`
- `packages/node-agent/test/imageView.test.mjs`

## Implementation scope

The pure-JavaScript codec validates PNG, JPEG, GIF, and WebP structures. PNG/JPEG inputs support bounded decode, proportional resize, PNG output, and the Rust JPEG quality sequence. GIF/WebP remain readable and return stable warnings when re-encoding would be required.

## Acceptance checklist

- [x] MIME type is detected from file content.
- [x] Width, height, original bytes, and resized state are returned.
- [x] Dimension limits trigger proportional resize when enabled for supported codecs.
- [x] Byte limits trigger bounded PNG/JPEG quality and format handling.
- [x] Unsupported resize formats return warnings or stable errors without corrupting output.

## Verification

Covered by generated PNG/JPEG fixtures, valid GIF/WebP fixtures, misleading extensions, truncated-container rejection, decode checks, source immutability, byte-cap fallback, native-binary scanning, and the full `npm run verify:repo` release check.
