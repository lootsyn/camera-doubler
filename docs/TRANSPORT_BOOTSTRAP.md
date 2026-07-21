# Receiver Transport Bootstrap Protocol

이 문서는 전체 설계서의 transport/Receiver 규범을 구현용으로 축약한다.

## 1. Transport topology

카메라마다 독립 SRT caller connection을 사용한다.

```text
port = SRT_BASE_PORT + stream_slot
```

Receiver는 같은 slot의 UDP port에서 listener로 기다린다. 한 port에 여러 카메라를 multiplex하지 않는다.

## 2. Canonical SRT stream ID

```text
rmc1;emb=<pct>;edge=<pct>;boot=<uuid>;sid=<uuid>;cid=<pct>;slot=<u16>;epoch=<u32>;role=<anchor|secondary>;codec=<h264|h265>;sig=<base64url>
```

- field order를 고정한다.
- string은 UTF-8 후 RFC 3986 percent-encoding한다.
- `sig` 제외 exact bytes에 HMAC-SHA256을 계산하고 앞 16 bytes를 base64url(no padding)로 표시한다.
- 최대 256 bytes다.
- SRT encryption은 passphrase와 함께 `pbkeylen=32`를 명시한다.
- session 변경 시 reconnect한다.
- `role`은 provisional hint이며 authoritative 값이 아니다.

## 3. Connection validation

caller 연결 callback에서 다음을 확인한다.

1. 문법, canonical encoding, 길이
2. HMAC
3. expected embodiment/edge policy
4. listen port와 slot 일치
5. codec 지원
6. duplicate camera/slot/session/epoch 충돌

검증 후에만 media pipeline을 만든다.

## 4. Bootstrap state machine

```text
LISTENING
→ TRANSPORT_IDENTIFIED
→ MEDIA_PROBING
→ PROVISIONAL_STREAM
→ MANIFEST_VALIDATED
→ DATASET_READY
```

Manifest 전에는 preview와 bounded raw recording만 가능하며 dataset step은 생성하지 않는다.

## 5. Authoritative anchor

최종 anchor는 `SessionManifestV1.anchor_camera_id`다. 다음 값을 교차 검증한다.

- manifest session/boot/edge/embodiment
- current stream camera ID
- slot, epoch, codec, transport port
- manifest camera role
- anchor context가 선언된 anchor에만 존재하는지

불일치는 session quarantine 사유다.

## 6. In-band extraction

```text
SRT → MPEG-TS demux → h264parse/h265parse alignment=au
    → SEI extractor → decoder
```

SEI extractor는 decoder 전 parser output에서 timestamp, anchor context packet, manifest chunks를 처리한다. Parser output caps는 `alignment=au`로 강제한다. Decoded image는 normalized AU PTS와 stream-local AU ordinal로 encoded metadata envelope와 결합한다. PTS missing/duplicate/ambiguous frame은 preview-only이며 dataset 결합에 사용하지 않는다.

## 7. Late join

Manifest chunk sequence는 첫 IDR, 주기 IDR, revision 변경, reconnect 이후 강제 IDR에서 시작하며 기본 한 AU에 한 chunk씩 후속 AU로 분산한다. Receiver는 manifest 전 context를 bounded queue에만 보관하며 timeout 후 폐기한다.

## 8. Archival

SRT stream ID는 TS payload에 포함되지 않으므로 connection identity를 `stream-envelope.json`으로 저장한다. 각 TS segment는 `segments/index.jsonl`에 first/last PTS와 capture timestamp, size, SHA-256을 기록한다. 프레임 timestamp와 state/action은 계속 SEI에서 복원한다.

## 9. Anchor AU hold and dataset cadence

Anchor encoded AU may arrive before the post-anchor RobotState bracket. The Edge holds only that anchor AU in a bounded queue until context is ready or the deadline expires. It never guesses context by nearest PTS. The virtual-camera and secondary-stream branches bypass this hold.

Receiver defaults to `anchor_native`: every accepted real anchor frame becomes one dataset step. `fixed_grid_nearest` is optional and selects real frames around the nominal grid without frame synthesis or reuse.
