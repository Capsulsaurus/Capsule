---
title: 사용자 가이드
description: capsule CLI로 Capsule 라이브러리를 일상적으로 사용하는 방법 — 가져오기, 백업, 검증, 정리, 안전한 삭제.
status: draft
---

이 가이드는 레퍼런스 클라이언트인 `capsule` CLI를 다룹니다. *(예정)* 표시가 붙은 명령은
[로드맵](https://github.com/Capsulsaurus/Capsule/blob/master/ROADMAP.md)에 있으며 아직 사용할
수 없습니다. 플래그에 관해서는 `capsule <command> --help`가 가장 정확하며, CLI
레퍼런스([](/reference/cli/))에 모든 플래그가 나와 있습니다.

:::caution
Capsule은 알파 단계입니다. 첫 도그푸드 릴리스 전까지는 가져오는 모든 원본의 독립적인 백업을
따로 보관하세요.
:::

<a id="set-up"></a>

## 설정

1. 라이브러리 만들기: `capsule library init --name "Photos"`. 라이브러리는 로컬에 있는 암호화된
   디렉터리이며, 그 암호 문구가 이 컴퓨터의 키를 보호합니다.
2. 서버에 로그인하기: `capsule auth register` 또는 `capsule auth login`(싱글 사인온에는 `--oidc`
   또는 `--device`).
3. 복구 설정하기: `capsule auth escrow`. 출력된 복구 비밀값을 오프라인에 보관하세요. 복구
   비밀값과 암호 문구가 모두 없으면, 서버는 아무것도 복호화할 수 없으므로 컴퓨터를 잃어버리는
   것은 곧 라이브러리를 잃어버리는 것입니다.

<a id="import-and-back-up"></a>

## 가져오기와 백업

- `capsule import <path>`는 파일을 라이브러리로 복사하고, 각 에셋의 출처와 메타데이터에
  서명하며, 썸네일을 생성합니다. `--provider takeout`은 Google Takeout 아카이브를 사이드카
  메타데이터와 함께 가져옵니다([](/guides/google-photos-migration/)).
- `capsule push`는 라이브러리를 업로드합니다. 업로드는 중단된 지점부터 다시 이어지므로 `push`를
  다시 실행해도 항상 안전합니다. 각 에셋을 올린 뒤 `push`는 서버에 모든 블롭을 영구적으로
  보관하고 있는지 확인을 요청하고, 서버가 서명한 보관 영수증을 라이브러리에 보관합니다. 실패한
  에셋은 보고되고 실행은 계속됩니다. 실패한 에셋이 하나라도 있으면 명령은 0이 아닌 코드로
  종료되며, `--json`은 각 에셋의 결과를 나열합니다.
- `capsule import --push`는 두 가지를 한 번에 수행합니다.
- **보관이 검증될 때까지 원본을 지우지 마세요.** 에셋은 서버가 영구 보관을 보고하고 서명된 보관
  영수증을 발급한 뒤에야 안전하게 저장된 것이며, `push`는 이를 verified로 보고합니다. 그 전에는
  원본 파일을 삭제하지 마세요. `import --move --push`는 각 원본을 그 에셋이 검증된 뒤에만
  삭제합니다. `--push` 없이 `--move`를 쓰면 모든 원본을 그대로 두고 그 사실을 알려 주며, 나중에
  같은 파일에 `import --move --push`를 실행하면 원본이 정리됩니다.
- `capsule restore`(*예정*)는 원본을 내려받아 가져올 때 서명된 내용과 바이트 단위로
  대조합니다.

<a id="organize"></a>

## 정리

- **라벨**은 자유 형식이며 에셋, 스택(RAW+JPEG 쌍, 연속 촬영, 브라케팅), 앨범, 이미지의 특정
  영역에 붙일 수 있습니다. 직접 추가한 라벨은 AI가 제안한 라벨과 분리되어 보관되며, 제안을
  승격하거나 무시할 수 있습니다. 에셋 라벨은 지금 사용할 수 있고, 스택·앨범·영역 라벨은
  *(예정)* 기능입니다.
- `capsule show <asset>`은 에셋의 서명된 메타데이터에 기록된 캡션, 별점, 라벨, 위치, 촬영
  시각을 출력합니다.
- `capsule repair capture-time`은 촬영 시각이 원본의 EXIF와 일치하지 않는 에셋을 찾아내고,
  `--apply`를 주면 이를 바로잡습니다. 위치 편집은 *(예정)* 기능입니다.

모든 편집은 서명된 추가 전용 변경입니다. 이전 값은 에셋의 출처 이력에 남고, 두 기기에서 동시에
한 편집은 결정적으로 병합됩니다.

<a id="delete-safely"></a>

## 안전하게 삭제하기

삭제하기 전에, 주변 에셋이 아직 맥락을 제공할 때 위치 정보 같은 메타데이터를 먼저 바로잡으세요.
자체 위치 정보가 없는 사진도 같은 산책에서 찍은 이웃 사진들이 라이브러리에 있는 동안에는 위치를
짐작하기 쉽지만, 그 사진들이 사라지면 맥락도 함께 사라집니다.

- `capsule cull`은 에셋을 채택(pick), 보통(neutral), 제외(reject)로 표시하고 표시별로
  필터링합니다.
- `capsule cull --sweep`은 제외된 에셋을 휴지통으로 옮깁니다. 휴지통의 에셋은 영구 삭제되기 전
  보존 기간(`--retain-days`) 동안 복구할 수 있습니다.
- `capsule trash delete --asset <id>`는 에셋을 라이브러리뿐 아니라 서버에서도 휴지통으로
  옮깁니다. 예를 들어 `capsule cull --sweep` 이후처럼 이미 휴지통에 있는 에셋은 휴지통에 들어갈
  때의 보존 기간을 유지합니다. `--retain-days`는 새로 삭제할 때만 적용되며, `--json`은 전송된
  기간을 `retention_until`로 보고합니다.
- `capsule trash restore --asset <id>`는 에셋을 되살립니다. 보존 기간이 끝난 뒤의 복원은
  거부되며(`--json`에서는 `retention_elapsed`) 재시도되지 않고, 에셋은 영구 삭제될 때까지
  휴지통에 남습니다.

<a id="share"></a>

## 공유

- `capsule share create --album <id>` 또는 `--asset <id>`는 보기 전용 링크를 발급합니다.
  `--protect`는 링크 암호 문구를, `--expires-at`은 만료 시각을 추가합니다. 링크는 키를 URL
  프래그먼트에 담고 있으며, 프래그먼트는 서버에 절대 전달되지 않습니다.
- `capsule share revoke <id>`는 링크가 더 이상 열리지 않게 합니다.

<a id="sync-another-machine"></a>

## 다른 컴퓨터와 동기화

- `capsule sync`는 서버에서 라이브러리의 변경 피드를 가져오고, `capsule list`는 그 결과로 받은
  내용을 보여 줍니다.
- `capsule auth recover`는 복구 비밀값으로 계정을 새 컴퓨터로 가져옵니다.
