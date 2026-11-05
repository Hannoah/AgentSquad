# AgentSquad

여러 프로젝트의 터미널 세션(에이전트 작업 공간)을 하나의 TUI에서 관리하는 런처.
등록해 둔 프로젝트를 골라 터미널을 띄우고, `n/p`로 오가며 돌린다.

## 설치 (Windows)

저장소 없이 바로 설치:

```powershell
Invoke-WebRequest -Uri https://raw.githubusercontent.com/Hannoah/AgentSquad/main/tools/install.ps1 -OutFile install.ps1
powershell -ExecutionPolicy Bypass -File .\install.ps1
```

설치 위치는 `%LOCALAPPDATA%\AgentSquad\bin`이며 사용자 PATH에 자동 등록된다.
터미널을 다시 열면 어디서든 `squad`를 쓸 수 있다.
특정 버전이 필요하면 `-Version v0.2.0`을 붙인다.

## 바로 테스트

```powershell
squad add C:\path\to\proj -- claude   # 에이전트 프로젝트 등록
squad add C:\path\to\api --kind service -- npm run dev  # 서버 프로젝트 등록
squad list                             # 등록 확인
squad start proj                       # 매니저 열고 바로 시작
```

## 매니저 키

| 키 | 동작 |
|----|------|
| `Enter` | 세션 시작 / 포커스 |
| `/` | 선택 세션에 한 줄 전송 |
| `f` | 프로젝트 필터 |
| `r` | 세션 재시작 |
| `n`/`p`, `↑`/`↓` | 선택 이동 |
| `x` | 세션 중지 |
| `Ctrl+G` | 매니저로 복귀 (터미널에서) |
| `?` | 도움말 |
| `q` | 종료 (자식 프로세스도 함께 종료, 데몬 없음) |

사이드바는 **Agents** / **Services** 두 그룹으로 나뉜다.
Service 행에는 실행 명령이 함께 표시된다.

## CLI 전체

```
squad                         매니저 열기
squad add <path> [--kind agent|service] [-- cmd args]  프로젝트 등록 (기본 셸)
squad list                    등록 목록 (이름, 종류, 경로, 명령)
squad remove <name>           등록 해제 (파일은 건드리지 않음)
squad start <name>            매니저 열고 해당 프로젝트 시작
squad --version
```

## 설정 파일

- `%LOCALAPPDATA%\AgentSquad\config.toml` (또는 `AGENTSQUAD_CONFIG` 환경변수 경로)
- 지워도 되는 파일이다. 삭제하면 빈 상태로 시작한다.

## 소스 빌드 (개발용)

Rust stable + Windows GNU 툴체인이 필요하다.
MinGW 준비부터 빌드까지 한 번에 하는 스크립트가 있다.

```powershell
powershell -ExecutionPolicy Bypass -File tools\cargo-windows.ps1 build
powershell -ExecutionPolicy Bypass -File tools\cargo-windows.ps1 test
```

## 라이선스

MIT. [LICENSE](LICENSE) 참조.
