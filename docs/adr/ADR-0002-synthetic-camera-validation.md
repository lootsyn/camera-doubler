# ADR-0002: WSL에서 synthetic camera로 codec 경로 검증

- 상태: Accepted
- 날짜: 2026-07-22

현재 WSL kernel에는 USB camera와 loadable v4l2loopback device가 없다. `videotestsrc`를 실제 H.264 encoder에 넣고, timestamp SEI 삽입, MPEG-TS mux/demux, decoder 전 SEI 추출, H.264 decode까지 수행하는 bounded executable을 release gate로 사용한다.

이 검사는 픽셀 source만 synthetic이며 production codec/metadata/parser plugins와 코드는 그대로 실행한다. USB identity/hotplug 및 physical `/dev/video*` sink는 별도 hardware gate다.
