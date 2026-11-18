# Claude 세션 기록 및 저장소 현황

## 1. 문서 상태

- 작성일: 2026-11-18
- 목적: `codex-session-selector`와 비슷한 UX를 Claude Code 세션에 제공하기
  전에, 실제 세션 기록의 구조와 이 저장소의 현재 상태를 사실로 고정한다.
- 후속 문서: [구현 계획](claude-port-implementation-plan.md)
- 이 문서의 수치는 아래 표본에서 측정한 값이다. 다른 사용자, 다른 Claude
  Code 버전에서는 달라질 수 있다.

### 1.1 분석 표본과 도구

| 항목 | 값 |
| --- | --- |
| 대상 디렉터리 | `~/.claude/projects` (프로젝트 8개) |
| 메인 세션 JSONL | 46개, 28,499줄 |
| 서브에이전트 JSONL | 41개, 5,091줄, 43.8MB |
| 서브에이전트 메타 | `.meta.json` 41개 |
| Claude Code 버전 | `2.1.311`(대부분), `2.1.322` |
| 구조 분석 | `dump-json-refs v0.2.0` |
| 값 분포 분석 | `profile-json-refs v0.1.2` |
| 파일 단위 사실 확인 | `jq` |

`profile-json-refs`는 stdin을 받지 않으므로 세션 파일을 한 파일로 이어
붙여 입력했다. 재현 절차는 10절에 있다.

## 2. 저장소 현황

### 2.1 이 저장소 (`claude-session-selector` 0.1.0)

`codex-session-selector`의 초기 버전에서 갈라진 포트이며 커밋은 2개다.

| 파일 | 줄 수 | 역할 |
| --- | --- | --- |
| `src/lib.rs` | 464 | 세션 파싱, 부분 문자열 검색, SQLite 재생성 |
| `src/bin/select-claude-session.rs` | 943 | selector TUI |
| `src/bin/record-claude-session-info.rs` | 92 | 인덱스 기록 |
| `src/bin/claude-replay-tui.rs` | 846 | replay TUI |

현재 동작과 한계:

- 바이너리가 3개로 나뉘어 있고, selector가 나머지 둘을 외부 명령으로
  실행한다.
- 인덱스는 매번 `sessions` 테이블을 지우고 다시 만든다. 증분 갱신,
  본문 검색(FTS), 실행 이벤트 저장이 없다.
- `sessions` 테이블은 `path, id, timestamp, cwd, repository_url, branch,
  first_message` 7개 컬럼이고 `PRAGMA user_version`은 0이다.
- 첫 유저 메시지는 `message.role == "user"`이고 `message.content`가
  문자열인 첫 레코드로 정한다. 루트 `type`을 확인하지 않는다(4.3절의
  문제에 해당).
- 서브세션은 경로에 `/subagents/`가 있는지로 판별한다.
- `y`는 `claude --resume <id>`를 복사한다.
- 기본 경로는 `~/.claude/projects`와 `~/claude-session-info.sqlite3`다.
  분석 시점에 이 DB 파일은 존재하지 않았다.

### 2.2 참조 구현 (`codex-session-selector` 0.3.4)

저장소 루트의 `codex-session-selector`는 `../codex-session-selector`를
가리키는 심볼릭 링크이며 git에 추적되지 않는다. 기준 커밋은
`eb63980329a888d7976aa98f879ba26c671bf24e`다.

| 기능 | 내용 |
| --- | --- |
| 바이너리 | `select-codex-session` 하나. 하위 명령 `index`, `replay` |
| 인덱스 | SQLite schema v6, STRICT 테이블 5개, 파일 지문(경로·크기·수정시각) 기반 증분 갱신 |
| 검색 | 메타데이터 부분 문자열 검색, FTS5/BM25 본문 검색 (`content:all/user/agent/exec`) |
| replay | 유저·에이전트·실행 항목, `e`로 실행 항목 토글 |
| 표시 옵션 | `--include-subsessions`, `--include-empty-messages`, `--include-exec` |
| 품질 게이트 | `scripts/check-before-commit.sh`, `.githooks/pre-commit`, `.github/workflows/ci.yml` |

소스는 약 1.2만 줄이다. Codex 기록 형식에 묶인 코드는 다음 네 곳이다.

| 위치 | Codex 형식 의존 |
| --- | --- |
| `src/session_event.rs` | `event_msg`, `response_item` 정규화와 첫 메시지 신뢰도 |
| `src/lib.rs` | `session_meta` 추출, 실행 이벤트 수집, `YYYY/MM/DD` 디렉터리 순회 |
| `src/replay/mod.rs` | `exec_command_end`, `function_call`, `custom_tool_call` 짝짓기 |
| `src/selector/mod.rs`, `src/cli.rs` | `codex resume <id>`, 바이너리·DB·루트 경로 이름 |

SQLite 저장 계층(`src/indexer/`), FTS, TUI 조작은 기록 형식과 무관하다.

## 3. 세션 저장 구조

```text
~/.claude/projects/
  <cwd의 '/'를 '-'로 바꾼 이름>/
    <sessionId>.jsonl                       메인 세션 (46)
    <sessionId>/subagents/agent-<id>.jsonl  서브에이전트 기록 (41)
    <sessionId>/subagents/agent-<id>.meta.json  서브에이전트 메타 (41)
    <sessionId>/tool-results/...            도구 출력 원본 (인덱싱 대상 아님)
    memory/...                              메모리 (인덱싱 대상 아님)
```

- 메인 세션 파일명과 내부 `sessionId`는 46개 모두 일치한다.
- 서브에이전트 41개의 부모 JSONL은 모두 존재한다.

## 4. 메인 세션 JSONL

### 4.1 루트 레코드 종류

루트 객체의 형태는 187종이지만 `type` 값은 20종이다.

| `type` | 건수 | 내용 |
| --- | --- | --- |
| `assistant` | 6,556 | 모델 응답. `message.content[]` 블록 |
| `user` | 3,830 | 사람 입력, 도구 결과, 시스템 주입 메시지 |
| `api-request` | 2,910 | API 요청 기록 |
| `attachment` | 2,783 | 환경·도구 목록 등 주입 정보 (`attachment.type` 33종) |
| `api-request-blob` | 2,451 | API 요청 본문. **`message` 필드를 가짐** |
| `last-prompt` | 1,498 | 마지막 프롬프트(`lastPrompt`, `leafUuid`) |
| `mode` | 1,473 | 모드 |
| `atis-latch` | 1,466 | 내부 상태 |
| `ai-title` | 1,433 | 세션 제목(`aiTitle`). 턴마다 다시 기록 |
| `permission-mode` | 1,348 | 권한 모드 |
| `system` | 1,161 | `turn_duration`, `stop_hook_summary`, `away_summary`, `local_command`, `informational`, `compact_boundary` |
| `file-history-snapshot` | 503 | 파일 이력 |
| `queue-operation` | 401 | 입력 대기열 |
| `agent-name` | 223 | 세션 이름(`agentName`) |
| `tools-used` | 165 | 사용 도구 목록 |
| `file-history-delta` | 102 | 파일 이력 |
| `cost-state` | 83 | 비용 집계 |
| `api-request-shape` | 58 | API 요청 형태 |
| `session-owner` | 46 | 계정 정보 |
| `continued-in` | 9 | 세션 이어가기(`continuedInSessionId`) |

용량은 `user` 28.0MB, `assistant` 27.4MB, `attachment` 12.1MB,
`api-request-blob` 10.9MB 순이다. 한 줄의 최대 길이는 631,750자다.

### 4.2 세션 메타

Codex의 `session_meta` 같은 헤더 레코드가 없다. 첫 레코드는 `mode`(41개)
또는 `ai-title`(5개)이다.

- `cwd`, `gitBranch`, `timestamp`, `version`, `sessionId`는 `user`,
  `assistant`, `attachment`, `system` 레코드마다 반복된다.
- `timestamp`가 없는 `type`: `ai-title`, `agent-name`, `last-prompt`,
  `mode`, `permission-mode`, `atis-latch`, `api-request-blob`,
  `file-history-snapshot`, `cost-state`, `tools-used`, `session-owner`.
- 한 파일 안에서 `cwd`가 바뀐다. 46개 중 33개는 1종, 13개는 2~10종이다.
- `entrypoint`는 전부 `cli`, `userType`은 전부 `external`, `isSidechain`은
  전부 `false`다. `sessionKind`는 값이 있으면 `bg`뿐이다(2,937건).
- repository URL 전용 필드는 없다. `user` 레코드(도구 결과)의
  `serverClassifierContext.context.git_state.visibility.origin`에
  `{"host": ..., "remote": ..., "visibility": ...}`가 있다. 2,821건 중
  2,413건에 값이 있고 408건은 `null`이다.

### 4.3 `message`를 가진 레코드

`message` 필드는 `user`, `assistant`뿐 아니라 `api-request-blob`에도 있다.

| 루트 `type` | `message.role` | 건수 |
| --- | --- | --- |
| `assistant` | assistant | 6,556 |
| `user` | user | 3,830 |
| `api-request-blob` | assistant | 1,562 |
| `api-request-blob` | system | 519 |
| `api-request-blob` | user | 370 |

루트 `type`을 먼저 확인하지 않고 `message.role`만 보면 같은 대화가 두 번
수집된다.

### 4.4 `user` 레코드

`message.content`는 문자열이거나 블록 배열이다.

| content | 블록 | 건수 | 내용 |
| --- | --- | --- | --- |
| 배열 | `tool_result` | 3,135 | 도구 결과. 루트에 `toolUseResult`도 있음 |
| 문자열 | - | 664 | 아래 표 |
| 배열 | `text` | 31 | `[Request interrupted by user]`(24), 스킬 본문 등 `isMeta: true`(7) |

문자열 content인 `user` 레코드의 분류:

| `origin.kind` | `promptSource` | `isMeta` | 건수 | 내용 |
| --- | --- | --- | --- | --- |
| `human` | `typed` | false | 407 | 직접 입력 |
| `human` | `suggestion_accepted` | false | 49 | 제안 수락 |
| `human` | `queued` | false | 14 | 대기열 입력 |
| `human` | 없음 | false | 3 | `<command-message>`로 시작 (슬래시 명령) |
| `task-notification` | `system` | false | 55 | `<task-notification>` |
| `peer` | `system` | true | 27 | 다른 세션이 보낸 메시지 |
| `auto-continuation` | `system` | true | 1 | 자동 이어가기 |
| 없음 | 없음 | true | 29 | `<local-command-caveat>` |
| 없음 | 없음 | false | 29 | `<command-name>` |
| 없음 | 없음 | false | 26 | `<local-command-stdout>` |
| 없음 | 없음 | true | 14 | `<system-reminder>` |
| 없음 | 없음 | false | 6 | 압축 요약 (`isCompactSummary: true`) |
| 없음 | 없음 | true | 4 | 기타 평문 |

- 사람이 입력한 프롬프트는 `origin.kind == "human"`으로 식별된다. 473건
  모두 content가 문자열이다.
- `isMeta`만으로는 걸러지지 않는다. `<command-name>`,
  `<local-command-stdout>`, `<task-notification>`은 `isMeta`가 `false`다.
- 46개 파일 중 7개는 `origin.kind == "human"`인 레코드가 한 건도 없다.

### 4.5 `assistant` 레코드

- `message.content[]`의 블록 타입: `tool_use` 3,134, `thinking` 2,601,
  `text` 828.
- 레코드 하나에 블록이 1개인 경우가 6,551건, 2개 3건, 3개 2건이다. 한
  응답의 블록들이 레코드 여러 개로 나뉘어 기록된다.
- `message.model`에 `<synthetic>`이 12건 있다.
- `message.stop_reason`: `tool_use` 5,827, `end_turn` 702.

### 4.6 도구 호출과 결과

| 도구 | 건수 |
| --- | --- |
| Bash | 2,724 |
| Write | 135 |
| Read | 102 |
| Edit | 62 |
| Agent | 48 |
| 기타 7종 | 63 |

- 호출: `assistant` 레코드의 `tool_use` 블록(`id`, `name`, `input`). Bash의
  `input`에는 `command`가 항상 있고 `description`, `timeout`,
  `run_in_background`가 선택적으로 있다.
- 결과: 다음 `user` 레코드의 `tool_result` 블록(`tool_use_id`, `content`,
  `is_error`). `content`는 문자열(3,046건)이거나 블록 배열(89건)이다.
  `is_error`가 `true`인 것은 102건이다.
- 루트의 `toolUseResult`는 객체 3,033건, 문자열 102건이다. Bash의 경우
  `stdout`, `stderr`, `interrupted`, `isImage`, `noOutputExpected`가 있다.

## 5. 서브에이전트 기록

### 5.1 메인과 같은 점

- `user`/`assistant`의 `message.content[]` 구조와 `tool_use`↔`tool_result`
  짝짓기가 같다. 도구 호출은 Bash 848건(전체 1,073건의 79%)이다.
- `assistant` 블록은 `tool_use` 1,073, `thinking` 615, `text` 32다. `text`
  블록이 적고, 최종 보고는 `SubagentHandback` 도구 호출의
  `input.message`에 담긴다(37건).
- `api-request-blob`(534건)에도 `message`가 있다.
- 메인 세션과 겹치는 `uuid`는 0건이다.

### 5.2 메인과 다른 점

| 항목 | 메인 | 서브에이전트 |
| --- | --- | --- |
| 루트 `type` | 20종 | 6종: `assistant` 1,720, `user` 1,173, `api-request` 866, `attachment` 748, `api-request-blob` 534, `api-request-shape` 50 |
| `ai-title`, `agent-name`, `last-prompt`, `continued-in` | 있음 | 없음 |
| `isSidechain` | 전부 `false` | 전부 `true` |
| `agentId` | 없음 | 모든 레코드에 있고 파일명과 일치 |
| `sessionId` | 파일명과 일치 | 부모 세션 id (예외는 6절) |
| 첫 레코드 | `mode` 또는 `ai-title` | 41개 모두 `user` |
| `origin.kind` | `human` 등 | `human` 없음. `coordinator` 9, `task-notification` 3 |
| `promptSource`, `turnOrigin` | 있음 | 없음 |
| 마지막 `assistant` 블록 | 일반 응답 | 35개가 `SubagentHandback` 도구 호출, 6개가 `text` |

첫 `user` 레코드는 41개 모두 content가 평문 문자열이고 `parentUuid`가
`null`, `isMeta`가 `false`, `origin`이 없다. 이것이 부모가 넘긴 위임
프롬프트다. 이후의 문자열 `user` 레코드는 전부 `isMeta: true`다.

`cwd`는 33개 파일이 1종, 8개가 2종이다. 첫 `cwd`가 부모 세션의 첫 `cwd`와
다른 파일이 26개다.

### 5.3 `.meta.json`

| 필드 | 값 |
| --- | --- |
| `agentType` | `general-purpose` 35, `claude-code-guide` 2, `statusline-setup` 2, `web-fetch` 2 |
| `description` | 위임 설명 문자열 (41개 모두 있음) |
| `toolUseId` | 부모의 `Agent` `tool_use` `id` (41개 모두 부모 JSONL에서 발견) |
| `spawnDepth` | 전부 1 |
| `requestShape` | `background` 39, `foreground` 2 |
| `finished` | `true` 39, 필드 없음 2 |
| 기타 | `isBuiltIn`, `launchedWithDefaults`, `requestNonInteractive`, `pickedEffortModel`, `model`, `spawnMode` |

수정시각은 JSONL과 같은 것이 34개, JSONL이 더 나중인 것이 5개, 메타가 더
나중인 것이 2개다.

## 6. 세션 이어가기 (`continued-in`)

이전 세션 A의 끝부분에 `continued-in` 레코드가 기록되고, 새 세션 B의 맨
앞에 A의 기록이 복사된다.

- `continued-in` 레코드는 9건이다. 대상 파일이 남아 있는 것은 5쌍이고,
  4건은 9~14줄짜리 세션에서 넘어간 것으로 대상 파일이 없다.
- 한 세션(`7bf2b89b`)은 `continued-in` 레코드가 2건이다.
- 5쌍 모두 공유 `uuid`가 B의 1번째부터 N번째까지 빈틈없이 이어진다.
- 복사된 레코드는 `uuid`와 `timestamp`가 원본과 같고 `sessionId`가 B의
  값으로 바뀐다(표본 1건 대조).
- A와 B 어느 쪽도 상대의 상위 집합이 아니다.

| A → B | A의 uuid | B의 uuid | 공유 | 첫 유저 메시지 |
| --- | --- | --- | --- | --- |
| `cfa6be64` → `0b655c20` | 943 | 945 | 940 | 같음 |
| `9a3f24a5` → `2ed58f09` | 898 | 730 | 668 | 다름 |
| `7be603f4` → `614923fd` | 741 | 835 | 651 | B에는 없음 |
| `99c57757` → `7be38401` | 113 | 257 | 107 | 같음 |
| `6d400182` → `1eb08b3c` | 18 | 30 | 18 | 같음 |

영향:

- 전체 `uuid` 14,307개 중 2,384개가 두 파일에 걸쳐 중복된다.
- 5쌍 중 2쌍은 B의 첫 `timestamp`가 A의 것과 같다.
- 서브에이전트 파일도 복사된다. 같은 이름의 `agent-<id>.jsonl`이 두 부모
  디렉터리(`7be38401`, `99c57757`)에 있는 경우가 2건이고, 이 파일들에서는
  내부 `sessionId`가 디렉터리의 부모 id와 다른 레코드가 312건이다.

## 7. Codex 기록과의 대응

| 개념 | Codex | Claude |
| --- | --- | --- |
| 세션 루트 | `~/.codex/sessions/YYYY/MM/DD/*.jsonl` | `~/.claude/projects/<project>/*.jsonl` |
| 세션 메타 | `session_meta` 레코드 1개 | 레코드마다 반복되는 필드의 첫 값 |
| 세션 id | `session_meta.payload.id` | 파일명 |
| 제목 | 없음 | `ai-title`, `agent-name` |
| repository URL | `session_meta.payload.git.repository_url` | `serverClassifierContext`의 `host`, `remote` |
| 유저 입력 | `response_item`(`user.text`), `event_msg/user_message` | `user` + `origin.kind == "human"` |
| 에이전트 응답 | `agent_message`, `response_item` assistant | `assistant`의 `text` 블록 |
| 명령 실행 | `exec_command_end`, `function_call` + output | `tool_use`(Bash) + `tool_result` |
| 서브세션 판별 | `session_meta`의 `source.subagent` 등 | 경로의 `/subagents/`, `isSidechain` |
| 서브세션 부모 | 기록 없음 | 경로의 부모 디렉터리명, 메타의 `toolUseId` |
| 세션 이어가기 | 없음 | `continued-in` + 기록 복사 |
| resume | `codex resume <id>` | `claude --resume <id>` (프로젝트 디렉터리 기준) |

## 8. 확정된 결정

구현 계획의 전제가 되는 사용자 결정이다.

1. `y`는 `cd <cwd> && claude --resume <id>`를 복사한다.
2. 세션 제목을 저장하고 표시한다. 스키마에 컬럼을 추가한다.
3. 이어받은 세션은 연결만 저장하고 표시한다. 중복 제거는 하지 않는다.
4. replay는 Codex UX와 같은 범위(유저·에이전트 텍스트, Bash 실행)로
   시작한다.
5. repository URL은 값이 있는 세션만 채운다.
6. 서브에이전트의 메타 파일은 증분 갱신의 지문에 넣지 않는다.
7. 서브세션의 부모 연결을 저장하고 "부모 세션으로 이동"을 제공한다.

## 9. 확인하지 않은 것

- `origin`, `promptSource` 필드가 없는 과거 Claude Code 버전의 기록.
  표본은 `2.1.311`과 `2.1.322`뿐이다.
- `.meta.json`의 `description`, `agentType`, `toolUseId`가 생성 후 바뀌지
  않는다는 것. 메타가 JSONL보다 나중에 수정된 2건의 원인은 추정
  (`finished` 기록)이다.
- `ai-title`과 `agent-name`의 관계와 우선순위.
- `spawnDepth`가 2 이상인 중첩 서브에이전트의 디렉터리 배치.
- 대상 파일이 없는 `continued-in` 4건의 원인.
- `CLAUDE_CONFIG_DIR` 등으로 세션 루트가 바뀌는 환경.
- `serverClassifierContext`는 이름상 내부 판정용 필드다. 향후 버전에서
  유지된다는 근거가 없다.

## 10. 재현 방법

```bash
work=/tmp/claude-session-analysis && mkdir -p "$work" && cd "$work"

# 메인 세션
find ~/.claude/projects -maxdepth 2 -name '*.jsonl' -print0 \
  | xargs -0 cat > main-all.jsonl
dump-json-refs main-all.jsonl --outdir refs-main --full-report main-report.txt
profile-json-refs main-all.jsonl --refs refs-main/schemas.sqlite -o profile-main.sqlite

# 서브에이전트
find ~/.claude/projects -path '*/subagents/*' -name '*.jsonl' -print0 \
  | xargs -0 cat > sub-all.jsonl
dump-json-refs sub-all.jsonl --outdir refs-sub --full-report sub-report.txt
profile-json-refs sub-all.jsonl --refs refs-sub/schemas.sqlite -o profile-sub.sqlite

# 필드별 값 분포 예시
sqlite3 -separator ' | ' profile-main.sqlite "
select s.canonical_path || '.' || f.field_name, v.value_text, sum(v.count)
from prof_shape s
join prof_shape_field f using (shape_id)
join prof_field_value v using (field_profile_id)
where s.canonical_path = '\$' and f.field_name in ('type', 'promptSource')
group by 1, 2 order by 1, 3 desc;"
```

`profile-json-refs`는 루트 `$`의 형태 수가 저장 한도(1,024)를 넘는다는
경고를 낸다. 이 문서에 인용한 필드의 값 분포는 모두 완전 분포
(`is_complete_distribution = 1`)였다.

`$.message.content[]` 경로의 프로파일 값에는 `api-request-blob`의 블록이
섞인다(4.3절). 4.5절, 4.6절, 5.1절의 블록·도구 건수는 루트 `type`을
`assistant` 또는 `user`로 거른 `jq` 집계다.

```bash
jq -r 'select(.type=="assistant") | .message.content[]?
       | select(.type=="tool_use") | .name' main-all.jsonl | sort | uniq -c
```
