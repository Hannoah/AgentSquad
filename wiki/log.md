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

## [2026-11-06] 기록 | 위키 생성
`wiki/` 폴더를 만들었다. 공개 저장소이므로 커밋과 푸시는 사람이 확인한 뒤에 한다. → [index.md](index.md)
