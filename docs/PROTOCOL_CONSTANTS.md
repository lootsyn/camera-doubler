# Protocol Constants and Canonical Encoding

machine-readable source는 `config/protocol_constants.toml`이다.

## SEI UUIDs

UUID bytes는 RFC 4122/network order로 H.264/H.265 `user_data_unregistered` SEI의 첫 16 bytes에 기록한다.

| Payload | UUID | Hex bytes |
|---|---|---|
| Sync timestamp | `4a1191e6-9578-53b3-92a7-04c049fe0d5b` | `4a1191e6957853b392a704c049fe0d5b` |
| Anchor context packet | `62ef08bb-2eb4-59fb-b83f-f8f874a80043` | `62ef08bb2eb459fbb83ff8f874a80043` |
| Session manifest chunk | `791a8fc5-d0c3-5abf-81da-abf7f0373194` | `791a8fc5d0c35abf81daabf7f0373194` |

## CRC

- Algorithm: CRC32C Castagnoli
- Anchor context: `AnchorFrameContextPacketV1.serialized_context` exact bytes
- Manifest: compression 전 complete serialized `SessionManifestV1` exact bytes

수신자는 CRC를 검증한 뒤 inner protobuf를 decode한다.

## Stream ID HMAC

Canonical field order:

```text
rmc1;emb=...;edge=...;boot=...;sid=...;cid=...;slot=...;epoch=...;role=...;codec=...
```

- string은 UTF-8 후 RFC 3986 percent-encoding한다.
- key와 enum 값은 ASCII lowercase다.
- `sig` 제외 exact bytes에 HMAC-SHA256을 계산한다.
- 앞 16 bytes를 base64url(no padding)로 encode해 `;sig=...`를 붙인다.
- duplicate key, unknown required key, non-canonical percent encoding은 reject한다.

## Schema IDs and bitmap order

- device/camera/feature descriptors use the deterministic ordering defined in the main design.
- observation/action schema JSON is canonicalized with RFC 8785 JCS.
- SHA-256 is calculated over UTF-8 canonical JSON; the first 8 bytes are interpreted as unsigned big-endian. Zero is reserved.
- `feature_validity_bitmap` uses `lsb0`: bit `i` corresponds to `feature_slices[i]`.
- manifest exact-byte CRC is independent of schema canonicalization.

## Runtime compatibility

- GStreamer 1.24+ is recommended for the multiple unregistered-SEI parser path.
- GStreamer 1.22/1.23 is accepted only with the custom codec parser fallback and a successful multiple-SEI round-trip startup test.
- zstd manifest decompression is bounded by declared size, total size, chunk count, time budget and compression ratio.
