# 개요

AgentSquad(`squad`)는 프로젝트마다 터미널 세션 하나를 한 화면에서 시작하고 관찰하고 조작하는 Windows, macOS용 TUI 관리자다. 만든 이유: 여러 터미널을 띄워 두고 에이전트와 개발 서버를 오가는 작업이 불편해서. 그래서 **반응속도와 민첩한 사용감이 가장 중요한 요구사항**이다([monitoring-and-performance.md](monitoring-and-performance.md)).

## 구조

- 바이너리 `squad`의 진입점은 `src/main.rs`(CLI 명령 처리)이고, 나머지는 모듈로 나뉜다.
  - `app`: 이벤트 루프. `app.rs`(상태, 틱 처리, `run`), `app/draw.rs`(그리기), `app/keys.rs`(키 입력), `app/mouse.rs`(마우스, 붙여넣기, 크기 변경), `app/actions.rs`(확인창, 우클릭 메뉴, 명령 하달, 브리핑).
  - `config`: 설정 파일과 프로젝트 모델(종류, 지휘 관계, 고정).
  - `session`: PTY 세션과 `SessionStatus`.
  - `sys`: 프로세스 트리, CPU/메모리 측정, 프로세스 종료. 플랫폼별 코드는 `cfg`로 나눈다.
  - `ui`: ratatui 렌더링과 `Mode`.
  - `updater`: GitHub Release 자동 업데이트.
  - `ledger`, `history`: 프로젝트의 `.squad/`(보고서, 작업 이력) 기록과 읽기. `h`로 이력을 본다.
  - `input`(입력 줄), `pathpick`(추가 폼의 경로 선택과 Tab 완성), `select`(터미널 텍스트 선택).
- 테스트는 `src/tests.rs`(공용 `use`와 헬퍼)와 `src/tests/`의 모듈별 파일로 나뉜다. 실제 `squad`를 pty에서 돌리는 e2e는 `tests/history_e2e.rs`(`cargo test --test history_e2e -- --ignored --nocapture`). 빌드 설정은 `Cargo.toml`. 모듈 분리 이력은 [log.md](log.md) 참고.
- 입력 모드는 `mode: Mode` 하나로 표현한다(Manager, Focus, Send, Filter, Add, Help). 확인 팝업은 별도 `confirm_*` 상태. `App`의 메서드는 위 파일들에 `impl App` 블록으로 나뉘어 있고, 파일 사이에서 부르는 메서드는 `pub(super)`다.
- 주요 의존성: ratatui(화면), crossterm(입력), portable-pty(세션 PTY), vt100(터미널 에뮬레이션), serde + toml(설정), anyhow. 플랫폼별로 Windows는 windows-sys, Unix(macOS)는 libc(프로세스 트리, 측정, 종료 신호).
- `portable-pty`는 0.8에 고정했다. 0.9는 콘솔이 없는 환경에서 시작 시 멈춘다(`Cargo.toml` 주석 참고).
- 세션은 PTY로 띄우고 출력은 읽기 스레드가 채널로 보낸다(채널 크기 32, 가득 차면 읽기 스레드가 대기).
- 종료 시 프로세스 트리 전체를 죽인다. Windows는 래퍼가 먼저 끝난 뒤 남은 손자 프로세스도 PID 스냅샷으로 찾아 종료한다. Unix는 프로세스 그룹 단위로도 종료하며, 이미 끝난 세션의 pid는 다른 프로세스가 쓸 수 있어 그룹 종료를 건너뛴다.

## 프로젝트 종류

| 종류 | 용도 | 경로 |
|---|---|---|
| Agent | 대화형 터미널(예: `claude`) | 필수 |
| Service | 장기 실행 프로세스(예: `npm run dev`) | 생략 가능. 생략하면 현재 디렉터리에서 실행하고 이름은 프로그램 이름 |

- 명령을 생략하면 기본 셸을 쓴다. Windows는 `pwsh.exe`(있으면) 또는 `powershell.exe`, macOS는 `$SHELL`.
- 에이전트는 `--under`/`u`로 보고 대상(지휘관)을 지정할 수 있다. `o`로 지휘관 밑 에이전트 전체에 명령을 하달한다.

## 설정

- 경로: `AGENTSQUAD_CONFIG`가 가리키는 파일이 우선이다. 없으면 Windows는 `%LOCALAPPDATA%\AgentSquad\config.toml`, macOS/Linux는 `$XDG_CONFIG_HOME` 또는 `~/.config` 아래 `AgentSquad/config.toml`.
- 같은 폴더의 `last-project` 파일이 종료 때 선택한 프로젝트를 기억한다.
- TUI(추가/삭제/편집)와 CLI(`add`/`remove`/`under`)가 같은 파일을 쓴다. 쓸 때 임시 파일에 쓴 뒤 rename한다.

## 저장소

- github.com/Hannoah/AgentSquad (공개). CI와 릴리스는 GitHub Actions. 설치는 [build-and-release.md](build-and-release.md) 참고.
- 지원 범위는 Windows(x86_64)와 macOS(Apple Silicon, Intel)다. CI는 `windows-latest`와 `macos-latest` 매트릭스로 돌고, 릴리스는 두 플랫폼 에셋을 올린다(macOS 지원 추가: c61110e).
- 현재 버전: v0.15.4.
