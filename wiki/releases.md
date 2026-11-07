# 릴리스 이력

2026-11-06 하루에 v0.5.1부터 v0.5.4까지 네 번 배포했다. v0.5.1~v0.5.3은 release 워크플로와 `main` CI가 성공했고 `squad-windows-x86_64.zip`이 올라간 것을 확인했다(v0.5.4의 결과는 [log.md](log.md) 참고). 최신이 위.

| 버전 | 변경 | 태그 커밋 |
|---|---|---|
| v0.6.1 | 기존 `config.toml`에 저장된 `command = ["cmd.exe"]` 항목을 로드 시 자동으로 PowerShell(`shell()`)로 마이그레이션하고, 기본 셸 Service가 `"cmd"`로 표시되던 문제 수정 | 태그 `v0.6.1` 참고 |
| v0.6.0 | 기본 셸을 PowerShell(`pwsh.exe`/`powershell.exe -NoLogo`)로 변경, 셸 기반 Service의 실시간 포그라운드 프로세스 이름 감지(`jarvis` 등), 작업 중 `?`(`NeedsInput`) 오탐 방지(Working 우선순위 및 코드/diff 필터링), 백그라운드 주기적 업데이트 감시(15분) 및 상태바 `⬆ (U)` 배지·`squad update` CLI 추가, SHA256 fail-closed 검증, `SessionStatus` enum 도입, 터미널 `RenderCache` 및 하단 12줄 쓰로틀 스캔, 병렬 세션 시작(`start_sessions`) 및 일괄 트리 종료(`shutdown_all`) | 태그 `v0.6.0` 참고 |
| v0.5.9 | 모드 불리언 5개를 `Mode` enum 하나로 통합(포커스 중 상태바 칩 클릭 시 도움말이 키를 가져가도록 수정), SHA256 코드를 `as_chunks`로 바꿔 최신 clippy `-D warnings` 통과 | 태그 `v0.5.9` 참고 |
| v0.5.6 | Agent 지능형 상태 감지(화면 텍스트 [y/n] 승인 대기, 작업 중 점스피너, 완료 체크마크), Service 타이틀 명령어 표기, 터미널 Synchronized Update(\x1b[?2026h/l)로 커서 깜빡임/튐 100% 차단, 터미널 스크롤바 인디케이터, 종료 배너 Esc/s 숨김, Caps Lock 사고 방지 전체 시작 확인 모달(S), 모달 백드롭 딤(Backdrop Dim) 및 Empty State 센터링 | 태그 `v0.5.6` 참고 |
| v0.5.5 | Service 이름을 폴더명이 아닌 실행 프로그램명으로 결정 및 동일 디렉터리 내 복수 Service 등록 지원(자동 유니크 네이밍 `prog`, `prog-2`), 마우스 컨트롤 전면 확장(사이드바 재클릭 시작/포커스, 우클릭 중지/시작 토글, 터미널 클릭 시작/포커스, 팝업 클릭 닫기/확인, 상태바 도움말), 커서 좌우 점프/플리커 버그 수정(BufWriter, Hide/Show, 200ms redraw, Run-length Span 병합), 사이드바 Running 텍스트 제거로 디테일 정보 노출 공간 확보, 프로세스 트리 종료 비동기화, \\?\ 저장 경로 정규화, 세련된 액센트 바(▎) 및 Empty State 카드 렌더링, 모달 Rounded 스타일 통일 | 태그 `v0.5.5` 참고 |
| v0.5.4 | 포커스 모드에서 `Esc`, `Ctrl+G`를 세션에 그대로 전달하고 이탈 키를 `Ctrl+Q`로 변경(접두사 모드 제거). 시작 시 새 버전 확인과 `y` 승인 업데이트 | 태그 `v0.5.4` 참고 |
| v0.5.3 | 세션별, PC 전체 CPU/메모리 모니터링. 측정은 백그라운드 스레드에서 2초마다 | `795eb3b` |
| v0.5.2 | 마우스 클릭 선택과 포커스. 종료된 세션에 배너 표시와 자동 포커스 해제. 도움말 팝업이 24줄 터미널에서 잘리던 문제 수정. 키맵 재설계 | `b640877` |
| v0.5.1 | TUI 프로젝트 삭제를 설정 파일에 저장(`Delete`는 삭제). 코드 리뷰 항목 수정: 설치 PATH 버그, 설정 파일 동시성과 원자적 쓰기, `\\?\` 경로, 유휴 CPU, F1~F12 전달, add 폼 따옴표, `Ctrl+G` 접두사, 필터 유지 | `acdb63a` |
| v0.5.0 | 리뷰 시작 시점의 릴리스. 경로 없는 서비스 허용, `Delete` 중지 키 추가 | `c87cece` |

## 메모

- v0.5.1 이전에는 `Delete`가 "중지" 키였다가 "프로젝트 삭제"로 바뀌었다. 이 변경은 v0.5.1에 포함됐다. v0.5.2에서 중지는 `s`로 옮겼다([keymap.md](keymap.md)).
- 각 릴리스의 상세 근거는 [review-findings.md](review-findings.md), 측정은 [monitoring-and-performance.md](monitoring-and-performance.md).
