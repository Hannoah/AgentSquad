# 빌드, 테스트, 배포

## 빌드와 테스트

- Windows에서 테스트와 빌드는 MinGW 링커가 필요하다. 헬퍼 `tools\cargo-windows.ps1`이 휴대용 툴체인을 받아 와서(처음 한 번 약 200MB) cargo에 인자를 넘긴다.
  - `powershell -ExecutionPolicy Bypass -File tools\cargo-windows.ps1 test --locked`
- 일반 `cargo test`는 `dlltool.exe: program not found`로 실패한다(`windows-sys` 빌드 단계). 반면 `cargo check`, `cargo clippy`는 일반 `cargo`로 된다.
- 헬퍼 스크립트는 `-D warnings`나 `--`를 `-File` 인자로 넘기면 `Missing an argument for parameter 'CargoArgs'`로 실패한다. clippy는 일반 `cargo clippy --all-targets -- -D warnings`로 돌린다.
- 테스트 77개(v0.5.3). 일부는 실제 프로세스를 띄워 트리 종료와 측정을 검증한다. 전체가 약 4~7초.
- 저장소 파일은 LF이고, 작업 트리에서 `LF will be replaced by CRLF` 경고가 나오지만 무해하다(autocrlf).

## CI

`ci.yml`(push to main, PR): `windows-latest`에서 `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --locked`.

## 릴리스 절차

`v*` 태그를 푸시하면 `release.yml`이 테스트, release 빌드, `squad-windows-x86_64.zip` 생성, GitHub Release 생성과 업로드를 한다.

1. `Cargo.toml`의 `version`을 올린다. `Cargo.lock`은 cargo가 갱신한다(워크플로가 `--locked`라서 lock 파일도 반드시 같이 커밋).
2. README의 `-Version vX.Y.Z` 예시를 새 버전으로 맞춘다.
3. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `tools\cargo-windows.ps1 test --locked`.
4. 커밋, `main`에 푸시, `git tag vX.Y.Z`, 태그 푸시.
5. 워크플로 결과와 릴리스 에셋을 확인한다.

이 PC에는 `gh`가 없어서 공개 API로 확인했다:
`https://api.github.com/repos/Hannoah/AgentSquad/actions/runs` 와 `.../releases/tags/vX.Y.Z`.

## 자동 업데이트 (다음 릴리스부터)

- 시작할 때 백그라운드 스레드가 새 버전을 확인한다. 화면과 입력은 기다리지 않는다. `https://github.com/Hannoah/AgentSquad/releases/latest`가 `/releases/tag/<태그>`로 리다이렉트되는 점을 이용해 `curl.exe`의 최종 URL만 읽는다(API 호출, 속도 제한, JSON 없음).
- 새 버전이 있으면 매니저에서 확인창을 띄운다. 포커스, 입력, 다른 확인창이 있으면 기다렸다가 키보드가 비었을 때 띄운다. 승인은 `y`만(`Enter`는 아님. 막 누르려던 Enter로 다운로드가 시작되는 사고를 막기 위해).
- 승인하면 백그라운드에서 `curl.exe`로 zip을 받고 `System32\tar.exe`(bsdtar)로 풀고, 새 바이너리의 `--version`이 태그와 같은지 확인한 뒤 교체한다. 실행 중인 세션은 그대로이고 다음 실행부터 새 버전을 쓴다.
- `tar.exe`는 PATH 대신 `System32` 전체 경로로 부른다. 이 PC는 Git의 GNU tar가 먼저 잡히는데 GNU tar는 zip을 못 푼다.
- 교체 방식: 실행 중인 exe는 이름 변경은 되지만 덮어쓰기와 삭제는 안 된다(이 PC에서 직접 확인). 그래서 `squad.exe`를 `squad.exe.old-<pid>`로 옮기고 새 파일을 복사한다. 복사가 실패하면 되돌린다. 옛 파일은 다음 실행 때 지운다(다른 창이 아직 쓰는 것은 못 지우고 남는다).
- 끄기: 환경 변수 `AGENTSQUAD_NO_UPDATE_CHECK=1`. 디버그 빌드는 확인도 교체도 하지 않는다(`cargo run`이 자기 자신을 덮어쓰지 않도록).
- 검증한 것: 버전 비교, `y`만 승인, 교체와 롤백(임시 파일), 실제 GitHub에서 v0.5.3 내려받기, 압축 해제, 버전 확인(`--ignored` 네트워크 테스트). 검증하지 못한 것: 화면의 확인창을 실제로 띄워 눌러 보는 것, 새 버전이 나온 상태에서의 전체 흐름.
- 한계: 체크섬 검증이 없다. HTTPS와 `--version` 일치 확인까지만 한다([open-work.md](open-work.md)).

## 업데이트(수동)

- 실행 중인 `squad`를 모두 종료한 뒤 설치 명령을 다시 실행한다. 설치 스크립트는 실행 중인 프로그램을 덮어쓸 수 없어서, 실행 중이면 중단하고 PID를 알려 준다.
- `irm https://raw.githubusercontent.com/Hannoah/AgentSquad/main/tools/install.ps1 | iex` 후 `squad --version`.
- 설치 스크립트는 사용자 PATH에만 설치 경로를 덧붙인다(v0.5.1에서 수정).

## 판단 근거

- 배포(커밋, 푸시, 태그)는 공개 저장소에 올라가고 되돌리기 어려워서, 승인 없이 하지 않고 매번 확인받았다.
- 릴리스에 체크섬이 없다. `irm | iex` 설치이므로 있으면 좋지만 보류했다([open-work.md](open-work.md)).
