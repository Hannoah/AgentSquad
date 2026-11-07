# 작업 기록

오래된 것이 위, 새 항목은 맨 아래에 추가한다. 형식: `## [날짜] 종류 | 제목`.

## [2026-11-06] 리뷰 | v0.5.0 전체 코드 리뷰
`src/main.rs` 전체와 CI, 설치 스크립트를 읽고 버그 4건, 사용성 항목, 구조 문제를 정리했다. clippy `-D warnings`와 fmt는 이미 깨끗했다. → [review-findings.md](review-findings.md)

## [2026-11-06] 배포 | v0.5.1
리뷰의 버그와 사용성 항목을 수정했다(PATH, 설정 동시성, 경로, 유휴 CPU, F키, 접두사, 필터, 따옴표). 테스트 69개. release와 CI 성공, 에셋 확인.

## [2026-11-06] 배포 | v0.5.2
마우스 클릭, 종료 배너, 도움말 크기, 키맵 재설계. `Delete`는 삭제, 중지는 `s`로 정리. 테스트 72개. release와 CI 성공, 에셋 확인. → [keymap.md](keymap.md)

## [2026-11-06] 배포 | v0.5.3
세션별, PC 전체 CPU/메모리 모니터링. 측정이 약 20ms 걸려 백그라운드 스레드로 분리. 테스트 77개. release와 CI 성공, 에셋 확인. → [monitoring-and-performance.md](monitoring-and-performance.md)

## [2026-11-06] 수정 | 포커스 모드에서 Esc를 세션에 전달
사용자가 발견: 에이전트 창에서 `Esc`가 필요한데 누르면 포커스가 풀렸다. `Esc`와 `Ctrl+G`를 세션에 그대로 전달하고 이탈 키를 `Ctrl+Q`로 바꿨다. 접두사 모드 제거. 테스트 77개 통과(이탈 키 테스트 교체). 아직 직접 눌러 보며 확인하지 못했다. → [keymap.md](keymap.md)

## [2026-11-06] 기능 | 시작 시 새 버전 확인과 업데이트
사용자 요청: 실행할 때 새 버전이 있으면 묻고 업데이트. 백그라운드 확인, `y`만 승인, 실행 중 exe는 이름 변경 후 교체. 테스트 80개 + 네트워크 테스트 2개 통과. 실제 화면 흐름은 새 버전이 나와야 확인 가능. → [build-and-release.md](build-and-release.md)

## [2026-11-06] 배포 | v0.5.4
`Esc` 전달과 `Ctrl+Q` 이탈, 시작 시 업데이트 확인을 묶어 배포. 테스트 80개 + 네트워크 테스트 2개 통과. v0.5.3 사용자는 자동 업데이트가 없으므로 이번 한 번은 수동 업데이트가 필요하다.

## [2026-11-06] 기록 | 위키 생성
`wiki/` 폴더를 만들었다. 공개 저장소이므로 커밋과 푸시는 사람이 확인한 뒤에 한다. → [index.md](index.md)

## [2026-11-06] 배포 | v0.5.5
Service 이름을 폴더명이 아닌 실행 프로그램명으로 지정하고 동일 경로 내 복수 Service 등록 허용(자동 disambiguation `name-2`). 마우스 제어 전면 확장(사이드바 클릭 시 선택/재클릭 시 즉시 시작 및 포커스, 우클릭 시 중지/시작 토글, 터미널 클릭 시 시작 및 포커스, 확인 모달 마우스 클릭 확인/취소, 상태바 칩 클릭으로 도움말 토글). 커서 좌우 깜빡임/점프 버그 수정(BufWriter, Hide/Show, 200ms redraw, Run-length Span 병합). 사이드바에서 `Running` 텍스트를 제거하고 스피너 기호로 압축하여 업타임/CPU/RAM/명령어 등 핵심 디테일 정보 노출 공간 확보. 프로세스 트리 종료 비동기화(`thread::spawn`), 저장된 `\\?\` 경로 자동 정규화, 세련된 액센트 바(`▎`), Empty State 카드 렌더링, 모달 Rounded 스타일 통일. 테스트 82개 통과. → [releases.md](releases.md)

## [2026-11-07] 기능 | Agent 지능형 상태 감지 및 UI/UX 마스터플랜 고도화
사용자 피드백 기반 UI/UX 전면 개편:
1. **Agent 지능형 상태 감지 및 직관적 기호 시각화**:
   - 단순 프로세스 `Running`을 넘어 실제 코딩 에이전트의 작업 국면을 감지하는 지능형 상태 머신(`AgentPhase`: `Working`, `NeedsInput`, `Done`, `Idle`) 도입.
   - 단어 설명(`Done` 등)을 배제하고 CLI 표준 기호만으로 표시: `?`(노랑 Bold, 승인 대기), `⠋`(Cyan 점스피너, 작업 진행 중), `✔`(초록 Bold, 작업 완료), `○`(회색, 대기).
   - 화면 하단 승인 프롬프트([y/n], approve, proceed 등) 감지로 사용자 승인 대기 즉시 노출.
2. **Service 타이틀 개선**:
   - Service의 터미널 메인 타이틀을 폴더명이 아닌 실제 실행 중인 서비스 명령어로 표시(`pane_title_name`).
   - v0.5.4 이하에서 등록된 서비스 폴더명 자동 마이그레이션(`load_from`).
3. **커서 좌우 점프/깜빡임 100% 차단**:
   - 터미널 diff redraw 직전 `\x1b[?2026h`(Synchronized Update Begin) 및 `cursor::Hide` 큐잉, 드로우 직후 `\x1b[?2026l` 전송으로 ConPTY 중간 상태 노출 완전 차단.
4. **터미널 스크롤바 인디케이터**:
   - 스크롤백 탐색 시 패널 우측에 세련된 스크롤바(`Scrollbar`) 트랙(`░`)과 썸(`█`) 렌더링.
5. **종료 배너 인터랙션 개선**:
   - 세션 종료 배너 안내를 `Enter/r restart · Esc/s dismiss`로 명확화하고 `Esc` 및 `s`로 배너 숨김 지원(`dismissed_exit`).
6. **Caps Lock 사고 방지 (`confirm_start_all`)**:
   - 대문자 `S` 입력 시 모든 정지 세션을 즉각 실행하지 않고 확인 모달을 띄워 오입력 방지.
7. **모달 백드롭 딤(Backdrop Dim) 및 Empty State 센터링**:
   - 모달 팝업 활성화 시 배경 화면 전체에 `DIM` 스타일을 적용하여 팝업 시인성 및 집중도 극대화.
   - 프로젝트 미등록 시 안내 카드를 터미널 중앙에 보기 좋게 정렬.
8. **단위 테스트 및 품질 검증**:
   - 89개 단위 테스트(87 통과, 2 네트워크 무시) 성공, `cargo clippy --all-targets -- -D warnings` 및 `cargo fmt --check` 무결점 검증 통과. release 바이너리 빌드 및 `%LOCALAPPDATA%\AgentSquad\bin\squad.exe` 배포 완료.

## [2026-11-07] 아키텍처 및 커서 안정화 | 코드베이스 모듈화 및 커서 지터/플리커링 완전 박멸
1. **커서 깜빡임/요동(Jitter & Flicker) 근본 원인 분석 및 해결**:
   - **원인 1 (200ms 강제 리드로우)**: `status == "Running"`인 세션이 하나라도 있으면 포커스 여부와 무관하게 200ms마다 `dirty = true`가 발생하여 초당 5회의 전체 diff redraw가 실행됨. 사용자가 터미널 포커스 상태(`focus == true`)로 타이핑하거나 출력 대기 중일 때는 스피너 애니메이션 틱을 건너뛰도록 분기(`if !focus && last_anim_tick.elapsed() >= Duration::from_millis(200)`).
   - **원인 2 (Hide/Show 토글 루프)**: 매 프레임 직전 `cursor::Hide`를 호출하고 Ratatui `f.set_cursor_position()`이 커서를 다시 Show하면서 초당 수십 번의 Show/Hide 상태가 반복되어 커서가 난폭하게 점멸함. 수동 `cursor::Hide` 큐잉을 전면 제거하고 Ratatui 네이티브 커서 라이프사이클에 위임.
   - **원인 3 (버퍼 분리로 인한 커서 위치 유출)**: raw `stdout`으로 `\x1b[?2026h`를 전송하고 터미널 그리드는 `BufWriter`로 출력되면서 ConPTY에 중간 커서 이동 상태가 섞여 들어감. `terminal.backend_mut()`을 통해 동기화 시퀀스를 동일 버퍼 파이프라인으로 전송.
2. **5,300+ 라인 단일 파일 모듈화 분리**:
   - 거대했던 `src/main.rs`를 110라인의 간결한 진입점으로 축소하고 단일 책임 원칙에 따라 8개 전문 모듈로 재구성:
     - `src/config.rs`: 프로젝트 설정, `Kind`, `display_order`, 경로 정규화, 직렬화.
     - `src/sys.rs`: Windows Toolhelp32 프로세스 트리 순회, 백그라운드 CPU/메모리 측정.
     - `src/session.rs`: ConPTY 세션 관리, PTY 버퍼 읽기, `AgentPhase`, `SessionStatus`.
     - `src/input.rs`: 키 이벤트 처리, xterm 바이트 변환, `LineInput`, Shift+Enter 포워딩.
     - `src/ui.rs`: Ratatui 뷰 렌더링, 레이아웃, 모달, 상태 표시 배지.
     - `src/updater.rs`: 백그라운드 릴리스 확인, certutil SHA256 계산, 바이너리 핫스왑.
     - `src/app.rs`: 메인 이벤트 루프 및 TUI 수명주기 관리.
     - `src/tests.rs`: 89개 단위 테스트 모음.
3. **보안 무결성 강화 (SHA256 체크섬)**:
   - `.github/workflows/release.yml`: 릴리스 빌드 시 `squad-windows-x86_64.zip.sha256` 자동 생성 및 에셋 업로드.
   - `tools/install.ps1`: `Get-FileHash` 기반으로 다운로드된 zip의 SHA256 체크섬을 검증하여 압축 해제 전 변조/손상 차단.
4. **품질 검증**:
   - 89개 단위 테스트 100% 통과 (87 passed, 2 ignored).
   - `cargo clippy --all-targets -- -D warnings` 및 `cargo fmt --check` 경고 0건 통과.

## [2026-11-07] 기록 | 위키 정합성 점검
`overview.md`가 모듈 분리 전(단일 `main.rs`) 상태로 남아 있어 현재 구조로 고쳤다. index 기준 버전을 v0.5.8로 갱신. `Mode` enum 통합은 `app::run()` 약 1,700줄 재작성이라 기능 배포와 분리해 보류 중이다. → [open-work.md](open-work.md)
