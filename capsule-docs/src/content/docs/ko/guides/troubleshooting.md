---
title: 문제 해결
description: 보고서에 필요한 버전과 로그를 수집하고, Capsule 서버, 가져오기, 라이브러리에서 흔히 발생하는 문제를 해결합니다.
audit: none
---

문제가 생기면 여기서 시작하세요. 아래 내용으로 해결되지 않으면 [](/support/)에서 어디에
질문해야 하는지 알 수 있으며, 첫 번째 섹션의 진단 정보가 버그 보고서에 필요한 내용입니다.

<a id="collect-diagnostics"></a>

## 진단 정보 수집

<a id="version"></a>

### 버전

아직 릴리스가 공개되지 않았으므로 버전 번호로는 빌드를 구별할 수 없습니다. 모든 것이 소스에서
빌드되므로, 빌드한 커밋을 보고하세요. 체크아웃한 디렉터리에서 `git rev-parse --short HEAD`를
실행하면 됩니다. CLI에는 버전 플래그가 없으며, `capsule-server --version`은 모든 빌드가 공유하는
크레이트 버전만 출력합니다.

<a id="logs"></a>

### 로그

CLI와 서버는 로그를 **stderr**에 씁니다. stdout에는 명령이 출력하는 내용만 담기며, `--json`에서는
하나의 JSON 문서이므로 둘을 따로 저장할 수 있습니다.

```sh
RUST_LOG=debug capsule push --library ~/Photos.capsule 2> capsule.log
```

- `RUST_LOG`는 로그 양을 정합니다. 릴리스 빌드는 기본적으로 `info`, 디버그 빌드는 `debug`
  수준으로 기록합니다. 문제를 재현할 때는 `RUST_LOG=debug`를 쓰고, `RUST_LOG=trace`는 요청받았을
  때만 쓰세요. 양이 매우 많습니다.
- 서버의 로그 형식은 `LOG_FORMAT`으로 정합니다. 디버그 빌드에서는 기본값이 `pretty`, 릴리스
  빌드에서는 `json`입니다([](/development/local-development/#logs)).
- 전체 화면 TUI(터미널에서 인수 없이 실행한 `capsule`)는 자신이 그리는 화면에 로그를 남길 수
  없으므로, CLI의 캐시 디렉터리에 `tui.log`를 씁니다. 이 디렉터리는 `capsule status`가 Cache
  Directory로 출력합니다. 이 파일은 세션마다 덮어써지므로 TUI를 다시 시작하기 전에 복사해 두세요.
- `--json` 출력은 각 에셋의 결과를 보고하므로 명령이 무엇을 했는지에 대한 가장 정확한
  기록입니다. 명령이 실패를 보고하면 이를 첨부하세요.

**공유하기 전에 민감한 정보를 가리세요.** 암호 문구, 복구 비밀값, 토큰, 키, 서버 주소, 개인 파일
경로를 지우세요. 라이브러리 암호 문구나 복구 비밀값은 비공개 보고서를 포함해 어디에도 붙여 넣지
마세요.

<a id="the-server"></a>

## 서버

<a id="it-refuses-to-start"></a>

### 서버가 시작되지 않을 때

- **종료 상태 2와 함께 변수 이름이 출력됩니다.** 영구 저장 서버에는 `DATABASE_URL`, `BLOB_ROOT`,
  `JWT_ED25519_DER`, `ATTESTATION_KEY_SEED`가 필요하며, 빠진 변수를 한 번에 모두 알려 줍니다.
  [](/guides/self-hosting/#configure)를 참고하세요.
- **데이터베이스 스키마가 바이너리보다 오래되었습니다.** `serve`는 절대 마이그레이션하지
  않습니다. 업그레이드할 때마다 `serve` 전에 `capsule-server-migration up`을
  실행하세요([](/guides/self-hosting/#migrate-then-start)).
- **신뢰할 수 있는 프록시 설정이 거부됩니다.** `TRUSTED_PROXIES`는 `/0` 네트워크를 거부하고,
  `TRUSTED_PROXY_HEADER`는 `x-forwarded-for` 또는 `forwarded`만 받습니다.

실행 중인 서버는 stdout에 `listening on <url>`을 출력합니다.

<a id="clients-cant-sign-in-or-connect"></a>

### 클라이언트가 로그인하거나 연결할 수 없을 때

- `capsule server-info`는 서버가 사용자를 로그인시키는 방식, 즉 ID 공급자가 있는지와 비밀번호를
  거부하는지를 보여 줍니다. 비밀번호가 비활성화된 서버에 비밀번호로 로그인하면 설계상 실패합니다.
- 리버스 프록시 뒤에서는 프록시가 `TRUSTED_PROXIES` 또는 `TRUSTED_PROXY_HOPS`에 지정될 때까지
  모든 클라이언트가 프록시의 속도 제한을 공유합니다. 여러 사람이 동시에 속도 제한에 걸리면 이것부터
  확인하세요([](/guides/self-hosting/#configure)).

<a id="the-index-and-the-blob-store-disagree"></a>

### 색인과 블롭 저장소가 일치하지 않을 때

`capsule-server scrub`은 읽기 전용 무결성 검사이며, 무엇이든 발견하면 0이 아닌 코드로
종료합니다. 백업을 복원했거나 데이터베이스를 잃은 뒤에는 `capsule-server rebuild`가 색인을
소유자 원장과 비교합니다. `serve`를 멈추고 `capsule-server rebuild --apply`를 실행하기 전까지는
아무것도 바꾸지 않습니다([](/guides/self-hosting/#maintenance)).

<a id="imports-and-uploads"></a>

## 가져오기와 업로드

- **`push`가 도중에 멈췄습니다.** 다시 실행하세요. 업로드는 중단된 지점부터 이어지므로 다시
  실행해도 항상 안전합니다. 실패한 에셋이 있으면 `push`는 0이 아닌 코드로 종료하며, `--json`은
  각 에셋의 결과를 나열합니다([](/guides/user-guide/#import-and-back-up)).
- **보관이 검증될 때까지 원본을 지우지 마세요.** `push`가 에셋을 verified로 보고하기 전에는 원본
  파일을 삭제하지 말고, 아니면 각 원본을 그 에셋이 검증된 뒤에만 삭제하는
  `import --move --push`를 사용하세요.
- **Google 포토 내보내기가 불완전해 보입니다.** Takeout 내보내기의 모든 부분을 내려받아 압축을
  풀었는지 확인하고, 가져오기 도구가 보장하지 못하는 부분을
  읽어 보세요([](/guides/google-photos-migration/#what-the-importer-cannot-guarantee)).
- **촬영 시각이 틀렸습니다.** `capsule repair capture-time`은 서명된 촬영 시각이 원본의 EXIF와
  일치하지 않는 모든 에셋을 보고하고, `--apply`는 이를 바로잡습니다. 각 수정은 영구적인 서명
  기록으로 남으므로 먼저 `--apply` 없이 실행하세요.

<a id="check-a-librarys-integrity"></a>

## 라이브러리 무결성 확인

사진이 사라졌거나 손상된 것 같다면, 원인을 찾는 동안 **멈추고 아무것도 삭제하지 마세요**.
삭제하면 복구에 필요한 맥락이 사라질 수 있습니다.

1. `capsule verify --library <path>`는 서버에 각 원본을 영구적으로 보관하고 있는지 묻습니다.
   `--deep`은 서버가 저장된 바이트가 있는지만 확인하는 대신 다시 해시하게 하고, `--signed`는
   서버의 서명된 판정을 라이브러리에 보관합니다.
2. 휴지통의 에셋은 보존 기간 동안 복구할 수 있습니다.
   `capsule trash restore --library <path> --asset <id>`로 되살리세요([](/guides/user-guide/#delete-safely)).
3. 기기나 키를 잃어버린 경우에는 별도의 절차가 있습니다. [](/design/backup-recovery/#recovery-paths)

데이터가 손실되거나 손상되었다면 각 단계의 출력을 첨부해 버그로 신고하세요. 데이터 손실이나 손상은
원인이 무엇이든 결함입니다.

<a id="still-stuck"></a>

## 그래도 해결되지 않을 때

[](/support/)에서 질문할 곳과, 바로 조치할 수 있는 버그 보고서를 작성하는 방법을 안내합니다.
