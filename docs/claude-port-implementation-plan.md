# Claude 세션 selector 구현 계획

## 1. 문서 상태와 목표 버전

- 상태: 구현 준비 완료. 승인 전에는 production code를 변경하지 않는다.
- 작성일: 2026-11-18
- 대상 package: `claude-session-selector` `0.2.0`
- 시작 baseline: 이 저장소 `0.1.0`(커밋 `78af318`)
- 이식 원본: `codex-session-selector` `0.3.4`
  (커밋 `eb63980329a888d7976aa98f879ba26c671bf24e`)
- 목표 SQLite schema: Claude schema v1
- 근거 자료: [Claude 세션 기록 및 저장소 현황](claude-session-status.md).
  이 계획의 "현황 N절"은 그 문서의 절 번호다.
- 작성 기준: 이식 원본의
  `docs/implementation-plan-authoring-guidelines.md`

## 2. 목표와 최종 동작

`select-claude-session` 한 바이너리가 `~/.claude/projects`의 세션을
`~/claude-session-info.sqlite3`에 증분 인덱싱하고, selector TUI에서 찾고,
replay TUI로 다시 본다. 조작과 화면은 `select-codex-session` 0.3.4와 같다.

```text
~/.claude/projects/**.jsonl  ->  ~/claude-session-info.sqlite3  ->  selector / replay
```

사용자에게 보이는 최종 동작:

- `select-claude-session`: 인덱스를 갱신하고 selector를 연다.
- `select-claude-session index`: 인덱스를 갱신하거나 다시 만든다.
- `select-claude-session replay [PATH]`: JSONL 하나를 replay한다.
- selector의 키, 검색 문법, `--include-subsessions`,
  `--include-empty-messages`, `--include-exec`, `--print-path`,
  `--no-refresh`, `--quick-check`, `--db`는 이식 원본과 같다.

이식 원본과 다른 동작(의도적 변경):

| 항목 | 동작 |
| --- | --- |
| 목록의 제목 | 메타데이터 줄에 세션 제목을 표시하고 메타데이터 검색 대상에 포함한다 |
| `y` | `cd '<cwd>' && claude --resume <id>`를 복사한다. 서브세션 행에서는 부모 세션의 명령을 복사한다 |
| `p` | 서브세션 행에서 부모 세션 행으로 선택을 옮긴다 |
| `[CONT]` 배지 | 다른 세션으로 이어진 세션 행에 표시한다 |
| replay의 에이전트 항목 | `assistant`의 `text` 블록과 `SubagentHandback`의 `input.message` |
| replay의 실행 항목 | `Bash` 도구 호출과 그 결과 |

## 3. 구현 범위와 명시적 비범위

범위:

- 이식 원본의 소스, 테스트, 스크립트, hook, CI를 이 저장소로 가져오기
- Claude 기록 정규화: 세션 메타, 첫 유저 메시지, 메시지, Bash 실행
- `~/.claude/projects` 순회와 서브에이전트 메타 파일 읽기
- Claude schema v1: `sessions`에 컬럼 5개 추가
- selector: 제목 표시, `[CONT]` 배지, `y` 명령 변경, `p` 부모 이동
- replay: Claude 기록 지원
- README, MANUAL, CHANGELOG, LICENSE, package 정보

비범위:

- 이어받은 세션의 중복 제거. 메시지·실행 이벤트에 레코드 `uuid`를
  저장하지 않는다. 검색 결과 접기도 하지 않는다.
- `thinking` 블록, `Bash` 이외 도구(`Write`, `Read`, `Edit`, `Agent` 등)의
  인덱싱과 replay 표시
- `tool_result.is_error`의 저장과 표시
- 서브에이전트 메타의 `finished`, `requestShape`, `spawnDepth` 저장
- 메타 파일을 증분 갱신의 지문에 포함하는 것
- `origin` 필드가 없는 과거 Claude Code 기록에 대한 폴백 규칙
  (현황 9절: 표본 없음)
- `CLAUDE_CONFIG_DIR` 자동 인식. 경로 변경은 `index --sessions-root`로만
  한다.
- 이식 원본의 schema v1~v5 migration 코드
- crates.io 배포

## 4. 현재 동작 및 호환성 계약

### 4.1 이 저장소 0.1.0에서 바뀌는 것 (breaking)

| 0.1.0 | 0.2.0 |
| --- | --- |
| `record-claude-session-info ARGS` | `select-claude-session index ARGS` |
| `claude-replay-tui ARGS` | `select-claude-session replay ARGS` |
| `select-claude-session` (바이너리 3개 중 하나) | `select-claude-session` (유일한 바이너리) |
| `y`: `claude --resume <id>` | `y`: `cd '<cwd>' && claude --resume <id>` |
| 매번 `sessions` 테이블 재생성 | 증분 갱신 |

유지하는 것:

- 기본 DB 경로 `~/claude-session-info.sqlite3`
- 기본 세션 루트 `~/.claude/projects`
- `--db`, `--no-refresh`, `--print-path`, `--record-command`,
  `--replay-command`

0.1.0이 만든 DB는 `PRAGMA user_version = 0`이고 `sessions` 테이블의 컬럼이
`path, id, timestamp, cwd, repository_url, branch, first_message`다. 이식
원본의 `is_legacy` 판정 조건과 정확히 같으므로 `SchemaState::Legacy`로
감지되어 JSONL에서 다시 만들어진다. 별도 migration은 없다.

### 4.2 이식 원본에서 유지하는 계약

- 증분 갱신: 지문은 경로, 크기, 수정시각(초·나노초). 변하지 않은 파일은
  다시 파싱하지 않는다. 삭제된 source의 행은 같은 트랜잭션에서 지운다.
- 모든 쓰기는 immediate 트랜잭션 하나에서 하고 외래 키와 무결성을
  검사한다.
- canonical 인덱스는 서브세션, 첫 메시지가 빈 세션, 실행 이벤트를 항상
  저장한다. `--include-*`는 보이는 집합만 바꾼다.
- `session_key`, `exec_key`, `message_key`는 일반 증분 갱신에서 유지된다.
- FTS5 contentless-delete 인덱스의 DDL, tokenizer, BM25 가중치, 질의 문법
- 더 새로운 schema는 덮어쓰지 않는다. 알 수 없는 schema는
  `index --rebuild`를 요구한다.
- JSON으로 파싱되지 않는 줄은 경고를 stderr에 쓰고 건너뛴다.

## 5. 확정한 기술·구조 결정

### 5.1 이식 방식

이식 원본의 파일을 복사해 이 저장소의 `src/`를 대체한다. git 이력은
가져오지 않는다. 이식 원본과 이 저장소는 같은 저자의 MIT 라이선스
코드이며 `LICENSE`를 함께 복사한다.

배제한 대안: 이 저장소의 0.1.0 코드에 기능을 하나씩 추가하는 방식. 증분
인덱스, FTS, 단일 바이너리, 실행 토글을 모두 다시 구현해야 한다.

### 5.2 레코드 선별은 루트 `type`으로 한다

`message` 필드는 `api-request-blob`에도 있다(현황 4.3절). 모든 정규화
함수는 루트 `type`이 `user` 또는 `assistant`인지 먼저 확인한다.

줄 하나가 최대 63만 자이고 전체의 절반 이상이 쓰지 않는 레코드이므로,
먼저 `type`만 읽고 필요한 레코드만 `serde_json::Value`로 파싱한다.

| 루트 `type` | 처리 |
| --- | --- |
| `user`, `assistant` | 전체 파싱. 메타, 메시지, 실행 이벤트 |
| `ai-title`, `agent-name`, `continued-in` | 전체 파싱. 세션 메타 |
| 그 밖의 값, `type` 없음 | 건너뜀 |

### 5.3 서브세션 판별은 경로로 한다

파일의 부모 디렉터리명이 `subagents`이고 파일명이 `agent-`로 시작하면
서브세션이다. 부모 세션 id는 `subagents`의 부모 디렉터리명이다. 내부
`sessionId`는 쓰지 않는다(현황 6절: 복사된 서브에이전트 파일에서 다르다).

레코드 단위 규칙(첫 메시지, 유저 메시지 수집)은 경로가 아니라 레코드의
`isSidechain`으로 판단한다. stdin으로 받은 replay 입력에는 경로가 없기
때문이다.

### 5.4 세션 메타의 출처

| 컬럼 | 메인 세션 | 서브세션 |
| --- | --- | --- |
| `id` | 파일명에서 `.jsonl` 제거 | 파일명에서 `agent-`와 `.jsonl` 제거 |
| `timestamp` | `user`/`assistant` 레코드의 첫 `timestamp` | 같음 |
| `cwd` | `user`/`assistant` 레코드의 첫 `cwd` | 같음 |
| `branch` | `user`/`assistant` 레코드의 첫 `gitBranch` | 같음 |
| `repository_url` | 첫 non-null `origin`의 `"{host}/{remote}"` | 같음 |
| `title` | 마지막 `ai-title.aiTitle`, 없으면 마지막 `agent-name.agentName` | 메타의 `description` |
| `parent_session_id` | NULL | 경로의 부모 세션 디렉터리명 |
| `parent_tool_use_id` | NULL | 메타의 `toolUseId` |
| `agent_type` | NULL | 메타의 `agentType` |
| `continued_in_session_id` | 마지막 `continued-in.continuedInSessionId` | NULL |

- `origin`은
  `serverClassifierContext.context.git_state.visibility.origin`이다.
  `host`와 `remote`가 모두 비어 있지 않은 문자열일 때만 채운다. 스킴은
  기록에 없으므로 붙이지 않는다(예: `git.example/team/repo`).
- 빈 문자열이나 공백뿐인 값은 없는 것으로 본다.
- 이어받은 세션 B의 `timestamp`는 복사된 첫 레코드의 값이므로 A와 같을
  수 있다. 그대로 둔다.

### 5.5 첫 유저 메시지

| 신뢰도 | 조건 |
| --- | --- |
| `Confirmed` | `type == "user"`, content가 문자열, `origin.kind == "human"`, `promptSource`가 문자열 |
| `Confirmed` | `type == "user"`, content가 문자열, `isSidechain == true`, `parentUuid`가 `null` |
| `Unsourced` | `type == "user"`, content가 문자열, `origin.kind == "human"`, `promptSource` 없음 |

이식 원본의 선택 규칙을 그대로 쓴다. 신뢰도가 더 높은 후보가 나오면
교체하고, 같은 신뢰도에서는 먼저 나온 것을 유지한다. 따라서 슬래시
명령(`Unsourced`)으로 시작한 세션은 이후 첫 직접 입력이 있으면 그것이
첫 메시지가 된다.

content가 공백뿐인 후보는 버린다. 문자열 접두사나 태그로 판별하는 규칙은
두지 않는다.

### 5.6 메시지 이벤트

| 역할 | 조건 | 본문 |
| --- | --- | --- |
| user | `type == "user"`, content가 문자열, `origin.kind == "human"` | content |
| user | `type == "user"`, content가 문자열, `isSidechain == true`, `isMeta != true` | content |
| agent | `type == "assistant"`, `message.model != "<synthetic>"`, `text` 블록이 1개 이상 | `text` 블록의 `text`를 `"\n"`으로 연결 |
| agent | `type == "assistant"`, `tool_use` 블록의 `name == "SubagentHandback"`, `input.message`가 문자열 | `input.message` |

- 본문이 공백뿐이면 이벤트를 만들지 않는다.
- 한 레코드에서 agent 조건 두 개가 모두 성립하면 `text` 본문 뒤에
  `"\n"`과 `input.message`를 붙여 이벤트 하나로 만든다.
- `SubagentHandback`을 포함하는 이유: 서브에이전트의 `text` 블록은 32건
  뿐이고 최종 보고는 이 도구 호출에 담긴다(현황 5.1절). 이것을 빼면
  서브세션의 결과가 검색과 replay에 나오지 않는다.
- `event_index`는 이식 원본과 같이 0부터 세는 줄 번호다.
- 이식 원본의 `NormalizedMessage.phase`는 Codex 전용이므로 제거한다.

### 5.7 실행 이벤트

- 호출: `type == "assistant"`인 레코드의 `tool_use` 블록 중
  `name == "Bash"`이고 `input.command`가 문자열인 것.
- 결과: `type == "user"`인 레코드의 `tool_result` 블록. `tool_use_id`로
  호출과 짝짓는다. 짝이 없는 결과는 버린다.
- 출력은 `tool_result.content`를 `value_to_text`로 평문화한 값이다. 루트의
  `toolUseResult`는 쓰지 않는다. 레코드 하나에 결과 블록이 여러 개일 때
  루트 값과 블록의 대응이 정해지지 않기 때문이다.
- 결과가 없는 호출의 출력은 빈 문자열이다.

| `exec_events` 컬럼 | 값 |
| --- | --- |
| `event_index` | 호출 레코드의 0 기준 줄 번호 |
| `call_id` | `tool_use.id` |
| `kind` | `"tool_use"` |
| `name` | `"Bash"` |
| `command` | `input.command` |
| `output` | 평문화한 결과 |

한 레코드의 블록은 표본 6,556건 중 5건만 2개 이상이고(현황 4.5절),
`UNIQUE (session_key, event_index)`를 유지하기 위해 레코드당 Bash 호출은
첫 번째만 수집한다. 버려진 호출이 있으면 파일당 한 번 stderr에 경고한다.

### 5.8 색인 대상 파일과 skip 규칙

- 대상: `<root>/<project>/*.jsonl`과
  `<root>/<project>/<dir>/subagents/agent-*.jsonl`. 그 밖의 깊이와
  디렉터리(`tool-results`, `memory`)는 순회하지 않는다.
- `user` 또는 `assistant` 레코드가 하나도 없는 파일은
  `ParseStatus::Skipped`다(이식 원본의 "`session_meta` 없음"에 대응).

### 5.9 서브에이전트 메타 파일

`agent-<id>.jsonl`을 파싱할 때 같은 디렉터리의 `agent-<id>.meta.json`을
읽는다.

- 파일이 없으면 세 컬럼을 NULL로 둔다. 경고하지 않는다.
- 읽기 오류나 JSON 오류면 stderr에 경고하고 세 컬럼을 NULL로 둔다.
  인덱싱은 계속한다.
- 지문에 포함하지 않는다. 메타만 바뀐 경우 인덱스는 다음에 JSONL이
  바뀌거나 `index --rebuild`를 할 때 갱신된다.

### 5.10 schema

Claude schema v1로 시작한다. 이식 원본의 v6에 `sessions` 컬럼 5개를
더한 구조이며, 이식 원본의 DB와는 호환되지 않는다(파일 경로가 다르다).

- 사용 기술: 이식 원본과 같은 `rusqlite 0.40.1` bundled SQLite, STRICT
  테이블, FTS5 contentless-delete(`FTS_MIN_SQLITE = 3.43.0`).
- FTS 테이블, trigger, `fts_sync_state`는 변경하지 않는다. `title`은 FTS에
  넣지 않고 메타데이터 부분 문자열 검색에만 넣는다.
- `SchemaState`는 `Empty`, `Legacy`, `Current`, `Future`, `Unknown`만
  남긴다. `Legacy`는 4.1절의 0.1.0 DB이며 rebuild한다.

### 5.11 selector 키

`p`는 이식 원본의 selector에서 쓰지 않는 키다(사용 중:
`q e / ? y j k d u g G h l 0 Tab Enter Esc`). 검색 입력 중에는 `e`와
마찬가지로 검색 문자로 처리한다.

## 6. 목표 파일 구조와 파일별 책임

```text
Cargo.toml                       package 0.2.0, bin select-claude-session
LICENSE, README.md, MANUAL.md, CHANGELOG.md
install-bundle.sh
scripts/check-before-commit.sh   품질 게이트 (이식 원본 그대로)
scripts/install-git-hooks.sh
scripts/benchmark-fts.sh
.githooks/pre-commit
.github/workflows/ci.yml
src/main.rs                      run_from_args 호출
src/lib.rs                       SessionRow, 세션 파일 파싱, 경로 순회
src/session_event.rs             Claude 레코드 정규화 (순수 함수)
src/cli.rs                       인자 파싱, 기본 경로, help
src/application.rs               index / select / replay 실행 흐름
src/indexer.rs                   build_index, IndexMode
src/indexer/schema.rs            DDL, schema 감지와 검증
src/indexer/store.rs             트랜잭션 쓰기, 세션 로드
src/indexer/scan.rs              지문, 안정 파싱, scan plan
src/indexer/fts.rs               FTS (변경 없음)
src/indexer/search.rs            검색 (세션 행 매핑만 변경)
src/selector/mod.rs              selector TUI
src/replay/mod.rs                replay TUI
src/terminal.rs, src/ui.rs, src/ui_state.rs   (변경 없음)
src/test_support.rs              Claude 형식 fixture
tests/cli.rs, tests/fts_benchmark.rs
tests/fixtures/main-session.jsonl
tests/fixtures/subagent/agent-a1.jsonl
tests/fixtures/subagent/agent-a1.meta.json
docs/                            이 문서와 현황 문서
```

삭제: `src/bin/select-claude-session.rs`,
`src/bin/record-claude-session-info.rs`, `src/bin/claude-replay-tui.rs`.

저장소 루트의 심볼릭 링크 `codex-session-selector`는 커밋하지 않고
`.gitignore`에 `/codex-session-selector`를 추가한다.

## 7. 타입과 함수 시그니처

### 7.1 `src/lib.rs`

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub path: PathBuf,
    pub id: Option<String>,
    pub timestamp: Option<String>,
    pub cwd: Option<String>,
    pub repository_url: Option<String>,
    pub branch: Option<String>,
    pub first_message: String,
    pub is_subsession: bool,
    // 이하 Claude schema v1에서 추가
    pub title: Option<String>,
    pub parent_session_id: Option<String>,
    pub parent_tool_use_id: Option<String>,
    pub agent_type: Option<String>,
    pub continued_in_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SourceKind {
    Main { session_id: String },
    Subagent { parent_session_id: String, agent_id: String },
}

// 경로 모양만 본다. 파일을 열지 않는다. 확장자가 jsonl이 아니면 None.
pub(crate) fn classify_source(path: &Path) -> Option<SourceKind>;

// 이식 원본의 is_subsession_meta를 대체한다.
pub fn is_subsession_path(path: &Path) -> bool;

// root가 없으면 빈 Vec. 결과는 정렬된 상태다. 디렉터리 읽기 오류는 Err.
pub fn session_jsonl_paths(sessions_root: &Path) -> Result<Vec<PathBuf>>;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubagentMeta {
    agent_type: Option<String>,
    description: Option<String>,
    tool_use_id: Option<String>,
}

// 없는 파일은 Default. 읽기·JSON 오류는 stderr 경고 후 Default.
fn read_subagent_meta(jsonl_path: &Path) -> SubagentMeta;

// user/assistant 레코드가 없으면 Ok(None). 열기·읽기 오류는 Err.
pub(crate) fn parse_session_file_data(
    path: &Path,
    include_exec: bool,
) -> Result<Option<ParsedSessionFile>>;

// first_message, cwd, repository_url, branch, timestamp, date에 더해
// title과 agent_type을 포함한다.
pub fn searchable_text(row: &SessionRow) -> String;
```

`ExecEvent`, `MessageEvent`, `ParsedSessionFile`, `CollectOptions`,
`should_include_row`, `filter_sessions`, `session_date`는 이식 원본과 같다.

### 7.2 `src/session_event.rs`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordKind {
    User,
    Assistant,
    AiTitle,
    AgentName,
    ContinuedIn,
    Other,
}

// 루트 type만 읽는다. JSON 객체가 아니거나 문법 오류면 Err.
// type이 없거나 문자열이 아니면 Ok(RecordKind::Other).
pub(crate) fn record_kind(line: &str) -> serde_json::Result<RecordKind>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MessageRole { User, Agent }      // 이식 원본과 같음

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormalizedMessage {
    pub role: MessageRole,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum FirstUserMessageConfidence { Unsourced, Confirmed }

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FirstUserMessageCandidate {
    pub confidence: FirstUserMessageConfidence,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BashCall {
    pub call_id: String,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolOutput {
    pub call_id: String,
    pub output: String,
}

// 아래 함수는 모두 루트 type을 직접 확인한다. 조건에 맞지 않으면
// None 또는 빈 Vec을 돌려준다. 오류를 내지 않는다.
pub(crate) fn first_user_message_candidate(value: &Value) -> Option<FirstUserMessageCandidate>;
pub(crate) fn normalize_message_record(value: &Value) -> Option<NormalizedMessage>;
pub(crate) fn bash_calls(value: &Value) -> Vec<BashCall>;
pub(crate) fn tool_outputs(value: &Value) -> Vec<ToolOutput>;
pub(crate) fn repository_ref(value: &Value) -> Option<String>;
pub(crate) fn value_to_text(value: &Value) -> String;   // 이식 원본과 같음
```

### 7.3 `src/indexer/schema.rs`

```rust
pub(crate) const SCHEMA_VERSION: i64 = 1;

pub(crate) enum SchemaState {
    Empty,
    Legacy,
    Current { sessions_root: PathBuf },
    Future { version: i64 },
    Unknown { version: i64, reason: String },
}
```

`sessions` 테이블의 정확한 DDL이다. 나머지 테이블(`index_metadata`,
`source_files`, `exec_events`, `message_events`), index, FTS DDL은 이식
원본과 같다.

```sql
CREATE TABLE sessions (
    path TEXT NOT NULL,
    id TEXT,
    timestamp TEXT,
    cwd TEXT,
    repository_url TEXT,
    branch TEXT,
    first_message TEXT NOT NULL,
    session_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL UNIQUE
        REFERENCES source_files(source_key) ON DELETE CASCADE,
    is_subsession INTEGER NOT NULL
        CHECK (is_subsession IN (0, 1)),
    has_nonempty_first_message INTEGER NOT NULL
        CHECK (has_nonempty_first_message IN (0, 1)),
    title TEXT,
    parent_session_id TEXT,
    parent_tool_use_id TEXT,
    agent_type TEXT,
    continued_in_session_id TEXT,
    UNIQUE (path),
    CHECK ((is_subsession = 1) = (parent_session_id IS NOT NULL)),
    CHECK (is_subsession = 1 OR parent_tool_use_id IS NULL),
    CHECK (is_subsession = 1 OR agent_type IS NULL),
    CHECK (is_subsession = 0 OR continued_in_session_id IS NULL)
) STRICT;

CREATE INDEX IF NOT EXISTS sessions_parent_session_id_idx
    ON sessions(parent_session_id);
```

row identity는 이식 원본과 같다. `session_key`는 `source_key`당 하나이고
증분 갱신에서 `UPDATE`로 유지된다.

### 7.4 `src/selector/mod.rs`

```rust
// POSIX sh 단일 인용. 내부의 '는 '\'' 로 바꾼다.
fn shell_single_quote(value: &str) -> String;

// 메인 행: 그 행의 id와 cwd.
// 서브 행: all_rows에서 id == parent_session_id인 메인 행의 id와 cwd.
// id가 없거나 부모 행을 찾지 못하면 None.
// cwd가 없으면 "claude --resume <id>"만 돌려준다.
fn resume_command(row: &SessionRow, all_rows: &[SessionRow]) -> Option<String>;

enum ParentJump {
    Moved,          // 선택을 옮겼다
    NotSubsession,  // 선택한 행이 메인 세션이다
    NotInView,      // 부모가 현재 목록(필터 결과)에 없다
}

impl SelectorApp {
    fn jump_to_parent(&mut self) -> ParentJump;
}
```

### 7.5 `src/cli.rs`

구조체는 이식 원본과 같다. 기본값만 바뀐다.

```rust
// SelectOptions::defaults().db, IndexOptions::defaults().output
home.join("claude-session-info.sqlite3")
// IndexOptions::defaults().sessions_root
home.join(".claude").join("projects")
```

`--version` 출력은 `select-claude-session <version>`이다.

## 8. 주요 흐름 및 알고리즘

### 8.1 경로 분류와 순회

```rust
fn classify_source(path: &Path) -> Option<SourceKind> {
    if path.extension()?.to_str()? != "jsonl" { return None; }
    let stem = path.file_stem()?.to_str()?;
    let parent = path.parent()?;
    if parent.file_name()?.to_str()? == "subagents" {
        // <root>/<project>/<parent_session_id>/subagents/agent-<id>.jsonl
        let agent_id = stem.strip_prefix("agent-")?;
        let parent_session_id = parent.parent()?.file_name()?.to_str()?;
        return Some(SourceKind::Subagent {
            parent_session_id: parent_session_id.to_owned(),
            agent_id: agent_id.to_owned(),
        });
    }
    Some(SourceKind::Main { session_id: stem.to_owned() })
}

fn session_jsonl_paths(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if !root.exists() { return Ok(out); }
    for project in read_dirs(root)? {                 // 디렉터리만
        for entry in fs::read_dir(&project)? {
            let path = entry?.path();
            if path.is_file() && has_jsonl_extension(&path) {
                out.push(path);                       // 메인 세션
            } else if path.is_dir() {
                let subagents = path.join("subagents");
                if !subagents.is_dir() { continue; }
                for sub in fs::read_dir(&subagents)? {
                    let sub = sub?.path();
                    // agent-*.jsonl만. *.meta.json은 제외된다.
                    if sub.is_file() && matches!(
                        classify_source(&sub),
                        Some(SourceKind::Subagent { .. })
                    ) {
                        out.push(sub);
                    }
                }
            }
        }
    }
    out.sort();
    Ok(out)
}
```

### 8.2 세션 파일 파싱

```rust
fn parse_session_file_data(path: &Path, include_exec: bool)
    -> Result<Option<ParsedSessionFile>>
{
    let kind = classify_source(path);     // None이면 Main{stem}으로 취급
    let mut seen_conversation = false;    // user/assistant를 봤는가
    let mut timestamp = None; let mut cwd = None; let mut branch = None;
    let mut repository_url = None;
    let mut ai_title = None; let mut agent_name = None;
    let mut continued_in = None;
    let mut best_first = None;            // (line, candidate)
    let mut exec_events = Vec::new();
    let mut exec_by_call_id: HashMap<String, usize> = HashMap::new();
    let mut message_events = Vec::new();
    let mut dropped_bash_calls = 0usize;

    for (line_number, line) in reader.lines().enumerate() {
        let line = line?;                 // 읽기 오류는 Err (이식 원본과 같음)
        let line = line.trim();
        if line.is_empty() { continue; }

        let record = match record_kind(line) {
            Ok(RecordKind::Other) => continue,   // Value로 파싱하지 않는다
            Ok(record) => record,
            Err(err) => { warn_invalid_json(path, line_number, &err); continue; }
        };
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(err) => { warn_invalid_json(path, line_number, &err); continue; }
        };

        match record {
            RecordKind::AiTitle => set_last(&mut ai_title, &value, "aiTitle"),
            RecordKind::AgentName => set_last(&mut agent_name, &value, "agentName"),
            RecordKind::ContinuedIn =>
                set_last(&mut continued_in, &value, "continuedInSessionId"),
            RecordKind::User | RecordKind::Assistant => {
                seen_conversation = true;
                set_first(&mut timestamp, &value, "timestamp");
                set_first(&mut cwd, &value, "cwd");
                set_first(&mut branch, &value, "gitBranch");
                if repository_url.is_none() {
                    repository_url = repository_ref(&value);
                }
                if let Some(candidate) = first_user_message_candidate(&value)
                    && !candidate.content.trim().is_empty()
                {
                    best_first = prefer_first_user_message(best_first, line_number, candidate);
                }
                if include_exec {
                    let mut calls = bash_calls(&value).into_iter();
                    if let Some(call) = calls.next() {
                        exec_by_call_id.insert(call.call_id.clone(), exec_events.len());
                        exec_events.push(ExecEvent {
                            session_path: path.to_path_buf(),
                            session_id: None,            // 아래에서 채운다
                            event_index: line_number,
                            call_id: Some(call.call_id),
                            kind: "tool_use".to_owned(),
                            name: Some("Bash".to_owned()),
                            command: call.command,
                            output: String::new(),
                        });
                    }
                    dropped_bash_calls += calls.count();
                    for output in tool_outputs(&value) {
                        if let Some(&index) = exec_by_call_id.get(&output.call_id) {
                            exec_events[index].output = output.output;
                        }
                    }
                    if let Some(message) = normalize_message_record(&value) {
                        message_events.push(MessageEvent {
                            event_index: line_number,
                            role: message.role,
                            content: message.content,
                        });
                    }
                }
            }
            RecordKind::Other => unreachable!(),
        }
    }

    if dropped_bash_calls > 0 { warn_dropped_bash_calls(path, dropped_bash_calls); }
    if !seen_conversation { return Ok(None); }

    let (id, is_subsession, parent_session_id, meta) = match kind {
        Some(SourceKind::Subagent { parent_session_id, agent_id }) =>
            (agent_id, true, Some(parent_session_id), read_subagent_meta(path)),
        Some(SourceKind::Main { session_id }) =>
            (session_id, false, None, SubagentMeta::default()),
        None => (file_stem_lossy(path), false, None, SubagentMeta::default()),
    };
    let row = SessionRow {
        path: path.to_path_buf(),
        id: Some(id),
        timestamp, cwd, repository_url, branch,
        first_message: best_first.map(|(_, c)| c.content).unwrap_or_default(),
        is_subsession,
        title: if is_subsession { meta.description } else { ai_title.or(agent_name) },
        parent_session_id,
        parent_tool_use_id: if is_subsession { meta.tool_use_id } else { None },
        agent_type: if is_subsession { meta.agent_type } else { None },
        continued_in_session_id: if is_subsession { None } else { continued_in },
    };
    for event in &mut exec_events { event.session_id = row.id.clone(); }
    Ok(Some(ParsedSessionFile { row, exec_events, message_events }))
}
```

`set_first`는 대상이 `None`이고 필드가 공백이 아닌 문자열일 때만 채운다.
`set_last`는 필드가 공백이 아닌 문자열이면 항상 덮어쓴다.

### 8.3 레코드 정규화

```rust
#[derive(Deserialize)]
struct RecordHead<'a> {
    #[serde(rename = "type", borrow, default)]
    kind: Option<Cow<'a, str>>,
}

fn record_kind(line: &str) -> serde_json::Result<RecordKind> {
    // 나머지 필드는 serde가 값을 만들지 않고 건너뛴다.
    let head: RecordHead = serde_json::from_str(line)?;
    Ok(match head.kind.as_deref() {
        Some("user") => RecordKind::User,
        Some("assistant") => RecordKind::Assistant,
        Some("ai-title") => RecordKind::AiTitle,
        Some("agent-name") => RecordKind::AgentName,
        Some("continued-in") => RecordKind::ContinuedIn,
        _ => RecordKind::Other,
    })
}

fn root_type(value: &Value) -> Option<&str> { value.get("type")?.as_str() }
fn user_text(value: &Value) -> Option<&str> {
    if root_type(value)? != "user" { return None; }
    value.pointer("/message/content")?.as_str()       // 배열 content는 None
}
fn is_human(value: &Value) -> bool {
    value.pointer("/origin/kind").and_then(Value::as_str) == Some("human")
}
fn is_sidechain(value: &Value) -> bool {
    value.get("isSidechain").and_then(Value::as_bool) == Some(true)
}

fn first_user_message_candidate(value: &Value) -> Option<FirstUserMessageCandidate> {
    let content = user_text(value)?.to_owned();
    let confidence = if is_human(value) {
        match value.get("promptSource") {
            Some(Value::String(_)) => FirstUserMessageConfidence::Confirmed,
            _ => FirstUserMessageConfidence::Unsourced,
        }
    } else if is_sidechain(value) && matches!(value.get("parentUuid"), Some(Value::Null)) {
        FirstUserMessageConfidence::Confirmed
    } else {
        return None;
    };
    Some(FirstUserMessageCandidate { confidence, content })
}

fn normalize_message_record(value: &Value) -> Option<NormalizedMessage> {
    match root_type(value)? {
        "user" => {
            let content = user_text(value)?;
            let is_meta = value.get("isMeta").and_then(Value::as_bool) == Some(true);
            let accepted = is_human(value) || (is_sidechain(value) && !is_meta);
            (accepted && !content.trim().is_empty()).then(|| NormalizedMessage {
                role: MessageRole::User,
                content: content.to_owned(),
            })
        }
        "assistant" => {
            if value.pointer("/message/model").and_then(Value::as_str) == Some("<synthetic>") {
                return None;
            }
            let blocks = value.pointer("/message/content")?.as_array()?;
            let mut parts: Vec<&str> = Vec::new();
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => parts.extend(block.get("text").and_then(Value::as_str)),
                    Some("tool_use")
                        if block.get("name").and_then(Value::as_str) == Some("SubagentHandback") =>
                    {
                        parts.extend(block.pointer("/input/message").and_then(Value::as_str));
                    }
                    _ => {}
                }
            }
            parts.retain(|part| !part.trim().is_empty());
            (!parts.is_empty()).then(|| NormalizedMessage {
                role: MessageRole::Agent,
                content: parts.join("\n"),
            })
        }
        _ => None,
    }
}

fn bash_calls(value: &Value) -> Vec<BashCall> {
    if root_type(value) != Some("assistant") { return Vec::new(); }
    blocks(value).filter(|b| str_field(b, "type") == Some("tool_use")
                          && str_field(b, "name") == Some("Bash"))
        .filter_map(|b| Some(BashCall {
            call_id: str_field(b, "id")?.to_owned(),
            command: b.pointer("/input/command")?.as_str()?.to_owned(),
        }))
        .collect()
}

fn tool_outputs(value: &Value) -> Vec<ToolOutput> {
    if root_type(value) != Some("user") { return Vec::new(); }
    blocks(value).filter(|b| str_field(b, "type") == Some("tool_result"))
        .filter_map(|b| Some(ToolOutput {
            call_id: str_field(b, "tool_use_id")?.to_owned(),
            // content는 문자열이거나 {type:"text", text} 블록 배열이다.
            output: value_to_text(b.get("content").unwrap_or(&Value::Null)),
        }))
        .collect()
}

fn repository_ref(value: &Value) -> Option<String> {
    let origin = value.pointer(
        "/serverClassifierContext/context/git_state/visibility/origin")?;
    let host = origin.get("host")?.as_str()?.trim();
    let remote = origin.get("remote")?.as_str()?.trim();
    (!host.is_empty() && !remote.is_empty()).then(|| format!("{host}/{remote}"))
}
```

`blocks(value)`는 `message.content`가 배열이면 그 원소를, 아니면 빈
iterator를 돌려준다.

### 8.4 인덱스 쓰기

`store.rs`의 `insert_session`, `update_session`, `load_session_for_source`,
`search.rs`의 `map_session_row`, `load_sessions_with_view`가 다루는 컬럼
목록에 새 컬럼 5개를 같은 순서로 추가한다. 트랜잭션 경계, diff 적용
순서, FTS 동기화는 바꾸지 않는다.

`choose_mode`는 다음 표로 줄어든다.

| `SchemaState` | `--rebuild` 없음 | `--rebuild` |
| --- | --- | --- |
| `Empty`, `Legacy` | Rebuild | Rebuild |
| `Current` (root 같음) | Incremental | Rebuild |
| `Current` (root 다름) | Rebuild | Rebuild |
| `Unknown` | 오류: `index --rebuild` 안내 | Rebuild |
| `Future` | 오류 | 오류 |

### 8.5 selector

메타데이터 줄의 구성과 순서:

```text
[MAIN|SUB] [CONT] <date> | <title> | <cwd> | <branch> | <repository>
```

- `[MAIN]`/`[SUB]`는 이식 원본과 같이 `--include-subsessions`일 때만
  붙는다.
- `[CONT] `는 `continued_in_session_id`가 있는 행에 항상 붙는다. 색은
  `Color::Magenta`.
- `<title>`은 `Color::White`, 값이 없으면 `-`.

```rust
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

fn resume_command(row: &SessionRow, all_rows: &[SessionRow]) -> Option<String> {
    let target = if row.is_subsession {
        let parent_id = row.parent_session_id.as_deref()?;
        all_rows.iter().find(|candidate|
            !candidate.is_subsession && candidate.id.as_deref() == Some(parent_id))?
    } else {
        row
    };
    let id = target.id.as_deref().filter(|id| !id.trim().is_empty())?;
    Some(match target.cwd.as_deref().filter(|cwd| !cwd.trim().is_empty()) {
        Some(cwd) => format!("cd {} && claude --resume {id}", shell_single_quote(cwd)),
        None => format!("claude --resume {id}"),
    })
}

fn jump_to_parent(&mut self) -> ParentJump {
    let Some(row) = self.selected_row() else { return ParentJump::NotSubsession };
    let Some(parent_id) = row.parent_session_id.clone() else {
        return ParentJump::NotSubsession;
    };
    // 현재 보이는 목록(검색 필터 적용 후)에서만 찾는다. 검색어를 지우지 않는다.
    match self.filtered.iter().position(|candidate|
        !candidate.is_subsession && candidate.id.as_deref() == Some(parent_id.as_str()))
    {
        // j/k가 쓰는 기존 선택 이동 코드로 index를 선택하고 스크롤을 맞춘다.
        Some(index) => { self.select(index); ParentJump::Moved }
        None => ParentJump::NotInView,
    }
}
```

상태 줄 문구:

| 상황 | 문구 |
| --- | --- |
| `y` 성공 | `copied: <명령>` |
| `y`, 서브 행의 부모가 `all_rows`에 없음 | `parent session is not indexed in this view` |
| `p`, 메인 행 | `selected session has no parent` |
| `p`, 부모가 목록에 없음 | `parent session is not in the current list` |

help와 footer의 `y` 설명은
``copy `cd <cwd> && claude --resume <id>` ``로, `p` 설명은
`select the parent session of a subsession`으로 한다.

### 8.6 replay

`load_entries_from_str`의 입력 처리(JSONL 또는 JSON 배열, stdin)와
`ReplayApp`의 상태·키·렌더링은 바꾸지 않는다. `normalize_record`와 그
하위 함수(`normalize_response_item`, `extract_exec_command`,
`is_exec_tool_call` 등 Codex 전용)를 다음으로 대체한다.

```rust
fn load_entries(values: Vec<Value>) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut exec_entry_by_call_id: HashMap<String, usize> = HashMap::new();
    for value in values {
        // 메시지를 먼저, 같은 레코드의 Bash 호출을 그다음에 넣는다.
        if let Some(message) = session_event::normalize_message_record(&value) {
            entries.push(Entry {
                kind: match message.role {
                    MessageRole::User => EntryKind::User,
                    MessageRole::Agent => EntryKind::Agent,
                },
                summary: first_line(&message.content),
                detail: message.content,
            });
        }
        for call in session_event::bash_calls(&value) {    // replay는 전부 표시
            exec_entry_by_call_id.insert(call.call_id, entries.len());
            entries.push(Entry {
                kind: EntryKind::Exec,
                summary: first_line(&call.command),
                detail: format!("type: tool_use | name: Bash\n\n$ {}\n", call.command),
            });
        }
        for output in session_event::tool_outputs(&value) {
            if let Some(&index) = exec_entry_by_call_id.get(&output.call_id) {
                entries[index].detail.push_str(&format!("\n{}", output.output));
            }
        }
    }
    entries
}
```

`summary`와 `detail`의 꾸밈(접두 문구, 줄바꿈 처리)은 이식 원본의 exec
항목 서식을 유지하고, 위 코드의 문자열은 그 서식의 Claude 값을 나타낸다.
`first_line`은 이식 원본이 summary를 만드는 기존 함수를 쓴다.

## 9. 오류 처리, 상태 소유권, lifecycle

| 상황 | 동작 |
| --- | --- |
| 세션 루트 없음 | 세션 0개로 인덱싱 (이식 원본과 같음) |
| JSONL 열기·읽기 실패 | 인덱싱 중단, 트랜잭션 rollback |
| JSON이 아닌 줄 | stderr 경고, 그 줄만 건너뜀 |
| `user`/`assistant` 없는 파일 | `Skipped`로 지문만 저장 |
| 메타 파일 없음 | 관련 컬럼 NULL, 경고 없음 |
| 메타 파일 손상 | stderr 경고, 관련 컬럼 NULL |
| 파싱 두 번 동안 파일이 계속 바뀜 | 그 파일을 다음 갱신으로 미룸 (이식 원본과 같음) |
| 0.1.0 DB | `Legacy` → rebuild |
| `--no-refresh`인데 schema가 v1이 아님 | 오류와 갱신 안내 (이식 원본과 같음) |
| 클립보드 실패 | 상태 줄에 오류 표시, TUI 유지 (이식 원본과 같음) |

- 실행 중인 세션의 JSONL은 계속 늘어난다. 다음 갱신에서 지문이 달라져
  다시 파싱된다.
- `all_rows`와 `filtered`는 `SelectorApp`이 소유한다. `resume_command`는
  `all_rows`를, `jump_to_parent`는 `filtered`를 읽는다.
- replay에서 돌아올 때 검색어와 선택을 유지하는 동작은 이식 원본과 같다.

## 10. 단계별 구현 순서

각 단계는 독립적으로 품질 게이트를 통과하고 커밋 하나가 된다.

**Phase 0. 문서**
이 문서와 현황 문서를 커밋한다.

**Phase 1. baseline 이식**
1. `src/`를 비우고 이식 원본의 `src/`, `tests/`, `scripts/`, `.githooks/`,
   `.github/`, `install-bundle.sh`, `LICENSE`를 복사한다.
2. `Cargo.toml`을 이식 원본 기준으로 바꾼다: `name =
   "claude-session-selector"`, `version = "0.2.0"`, `autobins = false`,
   `[[bin]] name = "select-claude-session"`, description과 repository,
   keywords의 `codex`를 `claude`로.
3. 이름만 바꾼다: 바이너리 이름, `--version` 출력, 기본 DB 파일명, 기본
   세션 루트, 환경 변수 `CODEX_CLI_BIN_DIR` → `CLAUDE_CLI_BIN_DIR`.
4. `.gitignore`에 `/codex-session-selector`를 추가한다.
5. 이 단계에서 파서는 여전히 Codex 형식을 읽는다. 기존 테스트는 fixture를
   바꾸지 않고 통과해야 한다.

**Phase 2. Claude 기록 파싱**
`session_event.rs` 교체, `lib.rs`의 `classify_source`,
`session_jsonl_paths`, `parse_session_file_data`, `test_support.rs`와
`tests/fixtures/`의 fixture를 Claude 형식으로 교체. 이 단계의
`SessionRow`는 기존 8개 필드만 채운다(`title` 등은 Phase 3). replay는
Codex 형식 그대로 둔다.

**Phase 3. schema v1과 새 컬럼**
`SessionRow` 필드 5개, 메타 파일 읽기, DDL, `SchemaState` 축소와 v1~v5
migration 코드·테스트 삭제, `store.rs`/`search.rs`의 컬럼 목록,
`searchable_text`.

**Phase 4. replay**
`normalize_record` 계열을 8.6절로 교체하고 replay 테스트 fixture를 Claude
형식으로 바꾼다.

**Phase 5. selector UX**
메타데이터 줄(제목, `[CONT]`), `resume_command`, `p`, help·footer 문구.

**Phase 6. 문서와 package**
README, MANUAL, CHANGELOG, help 문구 점검, 임시 경로 설치 확인.

## 11. red/green 및 커밋 정책

green-only commit이다. 각 phase에서 새 테스트를 먼저 쓰고 대상 테스트만
실행해 예상한 이유로 실패하는지 확인한 뒤, 같은 커밋에 구현을 넣는다.
red 상태는 커밋하지 않는다.

Phase 1은 동작 변경이 없는 이식이므로 새 테스트 없이 기존 테스트의
이름 변경분만 갱신한다.

커밋 메시지:

```text
docs: add Claude session status and port plan
refactor: import codex-session-selector 0.3.4 as the single-binary baseline
feat: parse Claude Code session records
feat: add Claude schema v1 with title and session links
feat: replay Claude Code sessions
feat: show titles and add parent navigation in the selector
docs: prepare v0.2.0
```

## 12. pre-commit/CI 자동화

이식 원본의 구성을 그대로 쓴다. 검사 명령은
`scripts/check-before-commit.sh` 한 곳에만 있다.

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

- `.githooks/pre-commit`과 `.github/workflows/ci.yml`은 이 스크립트만
  호출한다.
- Phase 1에서 `bash -n scripts/*.sh install-bundle.sh`로 문법을 확인하고
  `scripts/install-git-hooks.sh`로 hook을 켠다.
- 로컬 toolchain은 `rustc 1.99.1`이고 이식 원본의 `rust-version`은 `1.97`
  이다. `rust-version`은 `1.97`을 유지한다.

## 13. unit/integration/manual test

### 13.1 fixture

`tests/fixtures/main-session.jsonl` (한 줄에 레코드 하나, 순서대로):

```text
1  {"type":"mode","mode":"normal","sessionId":"main-1"}
2  {"type":"api-request-blob","message":{"role":"user","content":"blob must be ignored"}}
3  {"type":"user","isSidechain":false,"isMeta":true,"message":{"role":"user","content":"<system-reminder>ignore</system-reminder>"},"timestamp":"2026-11-01T00:00:00.000Z","cwd":"/tmp/fixture","gitBranch":"main"}
4  {"type":"user","isSidechain":false,"origin":{"kind":"human"},"message":{"role":"user","content":"<command-message>init</command-message>"},"timestamp":"2026-11-01T00:00:01.000Z","cwd":"/tmp/fixture","gitBranch":"main"}
5  {"type":"user","isSidechain":false,"origin":{"kind":"human"},"promptSource":"typed","message":{"role":"user","content":"fix the parser"},"timestamp":"2026-11-01T00:00:02.000Z","cwd":"/tmp/other","gitBranch":"feature"}
6  {"type":"assistant","isSidechain":false,"message":{"role":"assistant","model":"claude-test","content":[{"type":"thinking","thinking":"hidden"}]},"timestamp":"2026-11-01T00:00:03.000Z"}
7  {"type":"assistant","isSidechain":false,"message":{"role":"assistant","model":"claude-test","content":[{"type":"text","text":"checking status"}]},"timestamp":"2026-11-01T00:00:04.000Z"}
8  {"type":"assistant","isSidechain":false,"message":{"role":"assistant","model":"claude-test","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"git status","description":"status"}}]},"timestamp":"2026-11-01T00:00:05.000Z"}
9  {"type":"user","isSidechain":false,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"clean","is_error":false}]},"toolUseResult":{"stdout":"clean","stderr":""},"serverClassifierContext":{"context":{"git_state":{"visibility":{"origin":{"host":"git.example","remote":"team/repo","visibility":"unknown"}}}}},"timestamp":"2026-11-01T00:00:06.000Z"}
10 {"type":"assistant","isSidechain":false,"message":{"role":"assistant","model":"claude-test","content":[{"type":"tool_use","id":"toolu_2","name":"Read","input":{"file_path":"/tmp/a"}}]},"timestamp":"2026-11-01T00:00:07.000Z"}
11 {"type":"user","isSidechain":false,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_2","content":[{"type":"text","text":"file body"}]}]},"timestamp":"2026-11-01T00:00:08.000Z"}
12 {"type":"assistant","isSidechain":false,"message":{"role":"assistant","model":"<synthetic>","content":[{"type":"text","text":"No response requested."}]},"timestamp":"2026-11-01T00:00:09.000Z"}
13 {"type":"ai-title","aiTitle":"old title","sessionId":"main-1"}
14 {"type":"ai-title","aiTitle":"Parser fix","sessionId":"main-1"}
15 {"type":"continued-in","sessionId":"main-1","continuedInSessionId":"main-2","timestamp":"2026-11-01T00:01:00.000Z"}
```

`tests/fixtures/subagent/agent-a1.jsonl`:

```text
1 {"type":"user","isSidechain":true,"agentId":"a1","parentUuid":null,"message":{"role":"user","content":"count the files"},"timestamp":"2026-11-01T00:00:10.000Z","cwd":"/tmp/fixture","gitBranch":"main"}
2 {"type":"user","isSidechain":true,"agentId":"a1","isMeta":true,"parentUuid":"u1","message":{"role":"user","content":"<system-reminder>ignore</system-reminder>"},"timestamp":"2026-11-01T00:00:11.000Z"}
3 {"type":"assistant","isSidechain":true,"agentId":"a1","message":{"role":"assistant","model":"claude-test","content":[{"type":"tool_use","id":"toolu_h","name":"SubagentHandback","input":{"message":"there are 3 files"}}]},"timestamp":"2026-11-01T00:00:12.000Z"}
```

`tests/fixtures/subagent/agent-a1.meta.json`:

```json
{"agentType":"general-purpose","description":"Count files","toolUseId":"toolu_parent","spawnDepth":1,"finished":true}
```

`test_support::SessionFixture`는 임시 루트 아래에
`<root>/-tmp-fixture/main-1.jsonl`,
`<root>/-tmp-fixture/main-1/subagents/agent-a1.jsonl`과 메타 파일을 쓰는
`write_main_session()`, `write_subagent_session()`,
`write_named_session(name, message)`를 제공한다.

### 13.2 unit test

`src/session_event.rs`:

| 테스트 | 입력 | 기대 |
| --- | --- | --- |
| `record_kind_reads_only_the_root_type` | fixture 1, 2, 3, 7, 13, 15행 | `Other, Other, User, Assistant, AiTitle, ContinuedIn` |
| `record_kind_rejects_invalid_json` | `{"type":` | `Err` |
| `typed_human_prompt_is_confirmed` | 5행 | `Confirmed`, `"fix the parser"` |
| `human_prompt_without_source_is_unsourced` | 4행 | `Unsourced` |
| `meta_and_blob_records_are_not_first_message_candidates` | 2, 3행 | `None` |
| `tool_result_user_record_is_not_a_candidate` | 9행 | `None` |
| `sidechain_root_prompt_is_confirmed` | 서브 1행 | `Confirmed`, `"count the files"` |
| `sidechain_meta_message_is_not_a_candidate` | 서브 2행 | `None` |
| `assistant_text_is_an_agent_message` | 7행 | `Agent`, `"checking status"` |
| `thinking_and_synthetic_records_are_not_messages` | 6, 12행 | `None` |
| `handback_message_is_an_agent_message` | 서브 3행 | `Agent`, `"there are 3 files"` |
| `blob_message_is_not_normalized` | 2행 | `None` |
| `bash_calls_ignore_other_tools` | 8, 10행 | `[BashCall{toolu_1, "git status"}]`, `[]` |
| `tool_outputs_flatten_string_and_block_content` | 9, 11행 | `"clean"`, `"file body"` |
| `repository_ref_joins_host_and_remote` | 9행 | `Some("git.example/team/repo")` |
| `repository_ref_requires_both_parts` | `origin: null`, `remote: ""` | `None` |

`src/lib.rs`:

| 테스트 | 기대 |
| --- | --- |
| `classify_source_distinguishes_main_and_subagent` | 메인 경로 → `Main{"main-1"}`, 서브 경로 → `Subagent{"main-1","a1"}`, `agent-a1.meta.json` → `None` |
| `session_jsonl_paths_lists_main_and_subagent_files_only` | `tool-results/x.jsonl`, `memory/`, `*.meta.json`이 결과에 없음. 결과는 정렬됨 |
| `main_session_row_uses_first_values_and_last_title` | `id = main-1`, `timestamp = ...00:00:00.000Z`, `cwd = /tmp/fixture`, `branch = main`, `repository_url = git.example/team/repo`, `first_message = "fix the parser"`, `title = "Parser fix"`, `continued_in_session_id = main-2`, `is_subsession = false`, 부모 컬럼 `None` |
| `agent_name_is_the_title_fallback` | `ai-title`이 없고 `agent-name`만 있는 파일 → `title = agentName` |
| `subagent_row_reads_path_and_meta` | `id = a1`, `is_subsession = true`, `parent_session_id = main-1`, `parent_tool_use_id = toolu_parent`, `agent_type = general-purpose`, `title = "Count files"`, `first_message = "count the files"`, `continued_in_session_id = None` |
| `missing_or_broken_meta_leaves_columns_empty` | 메타 삭제, 메타 내용 `{` → 두 경우 모두 `Ok(Some)`, 세 컬럼 `None` |
| `exec_events_pair_bash_calls_with_results` | 이벤트 1개: `event_index = 7`, `call_id = toolu_1`, `kind = tool_use`, `name = Bash`, `command = git status`, `output = clean` |
| `message_events_exclude_meta_blob_and_tool_results` | `(event_index, role)`이 정확히 `(3, User)`, `(4, User)`, `(6, Agent)` 세 개 |
| `file_without_conversation_is_skipped` | `mode`, `ai-title`만 있는 파일 → `Ok(None)` |
| `searchable_text_includes_title_and_agent_type` | `"parser fix"`, `"general-purpose"`로 `filter_sessions`가 각각 1행 |

`event_index`는 0 기준 줄 번호이므로 fixture 4행은 3, 5행은 4, 7행은 6,
8행은 7이다.

`src/indexer/schema.rs`, `store.rs`, `indexer.rs`:

| 테스트 | 기대 |
| --- | --- |
| `claude_0_1_database_is_detected_as_legacy` | 4.1절의 7컬럼 `sessions`만 있는 DB → `SchemaState::Legacy` |
| `legacy_database_is_rebuilt_from_jsonl` | 위 DB에 `index` 실행 → `user_version = 1`, 세션 2행 |
| `new_columns_round_trip_through_the_index` | fixture 인덱싱 후 `load_sessions_with_view` 결과가 파싱한 `SessionRow`와 같음 |
| `subsession_check_constraints_reject_inconsistent_rows` | `is_subsession = 0`에 `parent_session_id`를 넣는 `INSERT`가 실패 |
| `future_schema_is_rejected` | `user_version = 2` → `Future`, `--rebuild`에도 오류 |
| `meta_only_change_does_not_reparse` (negative) | 메타 파일만 수정 후 갱신 → parsed 0, `title` 그대로 |
| `duplicate_uuids_across_continued_sessions_are_both_indexed` (negative) | 같은 메시지를 가진 두 파일 → `message_events` 행이 양쪽에 모두 있음 |

이식 원본의 증분·FTS·검색 테스트는 fixture만 Claude 형식으로 바꿔
유지한다. v1~v5 migration 테스트(`schema_v1_migrates_*`,
`schema_v5_refreshes_*`, `canonical_v1_is_detected_as_migratable`,
`selector_no_refresh_rejects_schema_v1_with_action`)는 삭제한다.

`src/replay/mod.rs`:

| 테스트 | 기대 |
| --- | --- |
| `loads_user_agent_and_bash_entries` | 메인 fixture → kind 순서 `User, User, Agent, Exec` |
| `bash_output_is_attached_to_its_call` | Exec 항목 detail에 `git status`와 `clean`이 있음 |
| `non_bash_tools_and_thinking_are_not_entries` (negative) | `Read`, `hidden`이 어떤 항목에도 없음 |
| `subagent_handback_is_an_agent_entry` | 서브 fixture → `User, Agent`, 두 번째 detail이 `there are 3 files` |
| `loads_preprocessed_json_array_events` | 같은 레코드를 JSON 배열로 넣어도 결과가 같음 |

토글·스크롤·fullscreen 테스트는 fixture만 바꿔 유지한다.

`src/selector/mod.rs`:

| 테스트 | 기대 |
| --- | --- |
| `resume_command_changes_directory_first` | `cd '/tmp/fixture' && claude --resume main-1` |
| `resume_command_quotes_single_quotes_in_cwd` | cwd `/tmp/it's` → `cd '/tmp/it'\''s' && claude --resume main-1` |
| `resume_command_without_cwd_omits_cd` | `claude --resume main-1` |
| `subsession_resume_command_targets_the_parent` | 서브 행 → 부모의 cwd와 id |
| `subsession_resume_command_needs_the_parent_row` | 부모가 `all_rows`에 없음 → `None` |
| `metadata_plain_uses_requested_column_order` | `2026-11-01 \| Parser fix \| /tmp/fixture \| main \| git.example/team/repo` |
| `continued_badge_is_shown_without_subsession_option` | `[CONT] `로 시작 |
| `p_moves_selection_to_the_parent_row` | `ParentJump::Moved`, 선택이 부모 행 |
| `p_on_main_row_reports_no_parent` | `NotSubsession`, 선택 불변 |
| `p_reports_parent_outside_the_filtered_list` | 검색으로 부모가 걸러진 상태 → `NotInView`, 선택과 검색어 불변 |
| `p_is_search_text_while_searching` | 검색 입력 중 `p` → 검색어에 `p` 추가 |

### 13.3 integration test (`tests/cli.rs`)

| 테스트 | 기대 |
| --- | --- |
| `version_uses_the_integrated_binary_name` | stdout이 `select-claude-session 0.2.0` |
| `index_creates_schema_v1_with_content_fts` | `user_version = 1`, 5개 테이블과 `sessions_fts` 존재, `sessions`에 새 컬럼 5개 |
| `index_second_run_reports_zero_parsed_files` | 이식 원본과 같음 |
| `index_changed_and_deleted_files_report_incremental_counts` | 이식 원본과 같음 |
| `index_rebuild_forces_full_parse` | 이식 원본과 같음 |
| `root_help_lists_parent_and_resume_keys` | help에 `p`와 `claude --resume` 문구 |

### 13.4 manual smoke

실제 `~/.claude/projects`로 확인한다. DB는 임시 경로를 쓴다.

```bash
cargo build --release
bin=target/release/select-claude-session
db=/tmp/claude-smoke.sqlite3

time "$bin" index --output "$db" --rebuild     # 소요 시간을 기록한다
"$bin" index --output "$db"                    # parsed 0 (실행 중인 세션 제외)
sqlite3 "$db" "select is_subsession, count(*), sum(title is not null),
               sum(first_message != '') from sessions group by 1;"
"$bin" --db "$db" --no-refresh --include-subsessions
```

TUI에서 확인할 것:

1. 메인 행에 제목이 보이고, `/`로 제목 일부를 검색하면 걸린다.
2. `Tab`으로 `content:exec`를 고르고 실제로 실행했던 명령어의 단어를
   검색하면 해당 세션이 나온다.
3. `[SUB]` 행에서 `p`를 누르면 부모 `[MAIN]` 행으로 이동한다.
4. `[SUB]` 행에서 `y`를 누르면 부모 세션의 `cd ... && claude --resume ...`
   가 복사된다. 붙여 넣어 실행하면 그 세션이 열린다.
5. `[CONT]` 배지가 이어진 세션(현황 6절의 A 쪽)에 보인다.
6. `Enter`로 replay에 들어가 `e`로 Bash 항목이 켜지고 꺼진다. `q`로
   돌아오면 검색어와 선택이 유지된다.
7. `--include-subsessions` 없이 실행하면 서브세션과 배지 `[MAIN]`/`[SUB]`
   가 보이지 않는다.

성능 기준은 정하지 않는다. 표본(메인 46개와 서브 41개, 약 142MB)의
`index --rebuild` 소요 시간과 DB 크기를 Phase 3 커밋 설명에 기록한다.

## 14. package 및 문서 변경

- `Cargo.toml`: 10절 Phase 1의 값. dependency는 이식 원본과 같다
  (`rusqlite 0.40.1` bundled, `ratatui 0.30`의
  `unstable-rendered-line-info`).
- `README.md`, `MANUAL.md`: 이식 원본의 구성을 따르고 Claude 기준으로
  다시 쓴다. Compatibility 절에는 5.5절의 첫 메시지 규칙, 5.3절의
  서브세션 판별, 확인한 Claude Code 버전(`2.1.311`, `2.1.322`)을 적는다.
  MANUAL의 migration 절에는 4.1절의 표를 넣는다.
- `CHANGELOG.md`: `0.2.0` 항목 하나. breaking change를 먼저 적는다.
- `install-bundle.sh`: 바이너리 이름과 `CLAUDE_CLI_BIN_DIR`.
- 임시 경로 설치 확인:

```bash
CLAUDE_CLI_BIN_DIR=/tmp/claude-selector-bin ./install-bundle.sh
/tmp/claude-selector-bin/select-claude-session --version
```

## 15. 완료 조건

- 아래 명령이 모두 통과한다.

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release
git diff --check
```

- `cargo install --path .`가 `select-claude-session` 하나만 설치한다.
- 13.4절의 7개 항목을 실제 세션으로 확인했다.
- `src/`에 `session_meta`, `event_msg`, `response_item`, `codex resume`
  문자열이 남아 있지 않다.

```bash
! grep -rnE 'session_meta|event_msg|response_item|codex resume|\.codex' src tests
```

- 비범위 항목이 들어가지 않았다: `message_events`와 `exec_events`에
  `uuid` 컬럼이 없고, `sessions`에 `finished` 컬럼이 없고, replay에
  `thinking`과 `Bash` 이외 도구 항목이 없다(13.2절의 negative test).

## 16. 후속 작업 경계

각각 별도 계획 문서로 다룬다. 이 계획에서는 관련 schema나 상태를 미리
넣지 않는다.

- 이어받은 세션의 검색 결과 접기(레코드 `uuid` 저장과 질의 시점 중복
  제거)
- replay에 `thinking`과 `Write`/`Edit`/`Read`/`Agent` 표시
- 실패한 실행(`is_error`) 표시
- 서브에이전트 실행 상태(`finished`)와 메타 파일 지문
- `origin` 필드가 없는 과거 기록 지원(표본 확보 후)
- 부모 행에서 서브세션 목록으로 내려가는 이동
