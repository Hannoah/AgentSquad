# 개요

AgentSquad(`squad`)는 프로젝트마다 터미널 세션 하나를 한 화면에서 시작하고 관찰하고 조작하는 Windows용 TUI 관리자다. 만든 이유: 여러 터미널을 띄워 두고 에이전트와 개발 서버를 오가는 작업이 불편해서. 그래서 **반응속도와 민첩한 사용감이 가장 중요한 요구사항**이다([monitoring-and-performance.md](monitoring-and-performance.md)).

## 구조

- Rust 단일 파일 `src/main.rs`가 바이너리 `squad`가 된다. 빌드 설정은 `Cargo.toml`.
- 주요 의존성: ratatui(화면), crossterm(입력), portable-pty(세션 PTY), vt100(터미널 에뮬레이션), serde + toml(설정), anyhow, windows-sys(프로세스 트리, 측정).
- `portable-pty`는 0.8에 고정했다. 0.9는 콘솔이 없는 환경에서 시작 시 멈춘다(`Cargo.toml` 주석 참고).
- 세션은 PTY로 띄우고 출력은 읽기 스레드가 채널로 보낸다(채널 크기 32, 가득 차면 읽기 스레드가 대기).
- 종료 시 프로세스 트리 전체를 죽인다. 래퍼가 먼저 끝난 뒤 남은 손자 프로세스도 PID 스냅샷으로 찾아 종료한다.

## 프로젝트 종류

| 종류 | 용도 | 경로 |
|---|---|---|
| Agent | 대화형 터미널(예: `claude`) | 필수 |
| Service | 장기 실행 프로세스(예: `npm run dev`) | 생략 가능. 생략하면 현재 디렉터리에서 실행하고 이름은 프로그램 이름 |

## 설정

- `%LOCALAPPDATA%\AgentSquad\config.toml`, 또는 `AGENTSQUAD_CONFIG`가 가리키는 파일.
- TUI(추가/삭제)와 CLI(`add`/`remove`)가 같은 파일을 쓴다. 쓸 때 임시 파일에 쓴 뒤 rename한다.

## 저장소

- github.com/Hannoah/AgentSquad (공개). CI와 릴리스는 GitHub Actions. 설치는 [build-and-release.md](build-and-release.md) 참고.
- 지원 범위는 Windows뿐이다. CI, 릴리스, 테스트 모두 Windows 전용이다.
