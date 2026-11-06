# AgentSquad 위키

`squad`의 현재 상태, 결정, 남은 일을 모아 두는 위키다. 새 정보가 생기면 해당 페이지를 고치고 [log.md](log.md)에 한 줄을 추가한다. 코드와 git 기록에서 알 수 있는 것은 적지 않고, 거기서 알 수 없는 이유와 결정만 적는다.

기준: v0.5.5 (2026-11-06)

| 페이지 | 내용 |
|---|---|
| [overview.md](overview.md) | squad가 무엇이고 어떤 구조인지 |
| [releases.md](releases.md) | v0.5.0 ~ v0.5.3 버전별 변경 |
| [review-findings.md](review-findings.md) | 코드 리뷰에서 나온 항목과 처리 현황 |
| [keymap.md](keymap.md) | 현재 단축키와 바뀐 이력 |
| [monitoring-and-performance.md](monitoring-and-performance.md) | CPU/메모리 표시, 반응속도 측정과 설계 이유 |
| [build-and-release.md](build-and-release.md) | 빌드, 테스트, 배포, 업데이트 절차와 함정 |
| [open-work.md](open-work.md) | 보류한 일과 후속 후보 |
| [log.md](log.md) | 시간순 작업 기록 |

## 유지 규칙

- 한 페이지는 한 주제. 같은 사실을 두 페이지에 적지 않고 링크한다.
- 사실에는 근거(버전, 커밋, 측정값)를 붙인다. 확인하지 못한 것은 "미확인"이라고 적는다.
- 처리한 항목은 지우지 않고 상태를 바꾼다(고침 / 보류 / 안 함).
- 날짜는 절대 날짜(YYYY-MM-DD)로 적는다.
