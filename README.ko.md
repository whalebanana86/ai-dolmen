# Dolmen

[English](README.md) | 한국어

스키마 버전관리가 내장된 오픈소스 AI 네이티브 데이터베이스 클라이언트입니다.

> **상태:** 초기 개발 단계입니다. 아직 배포된 버전이 없습니다.

*돌멘(dolmen)* 은 "돌 테이블"이라는 뜻의 고인돌로, 수천 년을 버텨 온 거석 유적입니다. Dolmen이 만들어지는 한반도에는 전 세계 고인돌의 약 40%가 있습니다. Dolmen은 여러분의 테이블 이력도 그만큼 오래 남게 하려 합니다. 모든 스키마 변경이 Git에 기록됩니다.

## 계획된 기능

- **Git 기반 스키마 버전관리** — GUI나 SQL 에디터에서 바꾼 내용을 읽기 쉬운 스키마 스냅샷과 마이그레이션 파일로 만들어 저장소에 커밋합니다. 데이터베이스가 저장소와 어긋나면 드리프트로 알려 줍니다.
- **데이터를 이해하는 AI** — 스키마를 아는 자연어→SQL 변환, 쿼리 설명, 에러 수정을 사용자 본인의 API 키로 제공합니다. 스키마와 그 Git 이력, 코드베이스를 컨텍스트로 씁니다.
- **기본이 안전** — AI가 만든 쿼리는 읽기 전용으로 실행하고, 변경은 실행 전에 영향을 보여 주며, 운영 연결은 보호됩니다.
- **깊이** — 실행 계획 시각화와 성능 진단.

## 데이터베이스

| 데이터베이스 | 상태 |
| --- | --- |
| PostgreSQL | 첫 번째 목표 (v0.1) |
| MySQL, MariaDB | 계획 (v0.2) |
| Redis, Valkey | 계획 (v0.3) |

## 개발

준비물:

- Rust (stable)
- Node.js 20.19+ 또는 22.12+
- pnpm 9
- 플랫폼별 [Tauri 시스템 의존성](https://tauri.app/start/prerequisites/)

```sh
# 데스크톱 앱
cd app
pnpm install
pnpm tauri dev

# CLI
cargo run -p dolmen-cli -- --help
```

개발·테스트용 로컬 PostgreSQL (Docker 필요):

```sh
docker compose up -d   # localhost:55432에 PostgreSQL 18, 사용자·비밀번호 "postgres"
DOLMEN_TEST_PG_HOST=localhost DOLMEN_TEST_PG_PORT=55432 cargo test
```

`DOLMEN_TEST_PG_HOST`가 없으면 서버가 필요한 테스트는 건너뜁니다.

## 저장소 구조

```
crates/dolmen-core   공용 코어: 연결, 스키마 조회, 스키마 버전관리
crates/dolmen-cli    `dolmen` 명령줄 도구
app/                 데스크톱 앱 프런트엔드 (Tauri 2 + React)
app/src-tauri        데스크톱 앱 백엔드 (Rust)
```

## 라이선스

[Apache License 2.0](LICENSE)
