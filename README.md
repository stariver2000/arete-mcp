# AI 뭐 쓰지? MCP 서버

[![AI 뭐 쓰지? MCP connector](https://glama.ai/mcp/connectors/kr.ai.arete/arete/badges/score.svg)](https://glama.ai/mcp/connectors/kr.ai.arete/arete)

> 하려는 일을 말하면, 그 일에 쓸 AI 를 찾아 줍니다.

**AI 뭐 쓰지?**([arete.ai.kr](https://arete.ai.kr))는 AI 앱·에이전트·Skill·MCP 서버를 할 일과 분야별로 모아 둔 한국어 디렉터리입니다. 이 저장소는 그 디렉터리를 ChatGPT·Claude 같은 AI 가 직접 찾아 쓸 수 있게 여는 **원격 MCP 서버**의 소스입니다.

"회의록 요약해 주는 AI 있어?" 라고 물으면, 내 AI 가 웹 여기저기를 뒤지는 대신 이 서버에 물어 정리된 답을 가져옵니다. 앱이 한국어를 지원하는지, 무료로 쓸 수 있는지, 어디서 받는지, 처음에 무엇부터 해 보면 좋은지까지 함께 옵니다.

## 바로 연결하기 (설치 없음)

연결 주소 하나만 넣으면 됩니다.

```
https://arete.ai.kr/connect/mcp
```

- **Claude Code**: `claude mcp add --transport http arete https://arete.ai.kr/connect/mcp`
- **Claude (웹·앱)**: 설정 → 커넥터 → 사용자 지정 커넥터 추가 → 위 주소
- **그 밖의 MCP 클라이언트**: Streamable HTTP 방식으로 위 주소를 등록

MCP 를 쓰지 않는 AI 라면 대화창에 이렇게 말해도 됩니다.

> arete.ai.kr/ask 를 읽고 나를 도와줘

연결 안내 화면: <https://arete.ai.kr/connect>

## 할 수 있는 일

| 도구 | 하는 일 |
|---|---|
| `search` | 하려는 일을 문장 그대로 넣으면('유튜브 영상 자막 만들기') 맞는 AI 앱·MCP·Skill·에이전트를 관련 순서로 찾습니다 |
| `find_by_field` | 분야(영상, 글쓰기, 개발 …)별 인기 항목. 한국어 지원·무료 앱만 거를 수 있습니다 |
| `get_ranking` | 종류별 인기 순위 (일·주·월) |
| `get_trending` | 요즘 갑자기 뜨는 항목 |
| `get_item` | 한 항목의 자세한 정보: 한국어 지원, 무료 여부와 확인한 날, 돌아가는 기기, 받는 곳, 처음 해 볼 일, 설치 안내 |
| `list_fields` | 분야 목록과 분야마다 항목 수 |

답에는 항상 출처(AI 뭐 쓰지? arete.ai.kr)와 항목 링크가 붙습니다.

## 이렇게 물어보세요

- "회의 녹음을 글로 바꿔 주는 무료 AI 알려줘"
- "PPT 만들어 주는 AI 중에 한국어 되는 거"
- "요즘 뜨는 MCP 서버 뭐 있어?"
- "Cursor 랑 비슷한 거 찾아줘"

## 쓸 수 있는 양

| | 분당 | 하루 |
|---|---|---|
| 키 없이 (IP 마다) | 30번 | 300번 |
| 계정 키 사용 (키마다 분당, 회원마다 하루) | 120번 | 5,000번 |

Claude 앱의 커넥터처럼 여러 사람이 같은 출구 주소를 함께 쓰는 경우, 키 없는 요청은 그 주소 묶음이 함께 쓰는 한도 안에서 처리됩니다. 많이 쓰신다면 계정 키를 넣어 주세요.

계정 키는 [arete.ai.kr/connect](https://arete.ai.kr/connect) 에서 로그인한 뒤 무료로 만듭니다. `Authorization: Bearer <키>` 머리 또는 주소 뒤 `?key=<키>` 로 넣습니다.

## 개인정보

- 키 없이 쓰면 접속 IP 를 한도를 세는 데만 메모리에 최대 24시간 두고, 파일이나 DB 에는 저장하지 않습니다.
- 계정 키를 쓰면 키마다 하루에 부른 횟수(날짜·횟수)만 남기고 90일 뒤 지웁니다.
- 검색어 같은 질문 내용은 답을 찾으려고 AI 뭐 쓰지? API 로 보냅니다. 어떻게 다루는지는 처리방침에 있습니다.
- 자세한 내용: [개인정보처리방침](https://arete.ai.kr/privacy)

## 이 저장소에 대해

Rust 로 쓴 MCP 서버(Streamable HTTP)이며, arete.ai.kr 의 공개 연결 주소에서 지금 실제로 돌고 있는 코드입니다. 서버는 디렉터리 데이터를 가진 AI 뭐 쓰지? API 를 불러 답을 만듭니다. 그래서 이 코드만 따로 띄워서는 데이터를 얻을 수 없습니다. 쓰실 때는 위의 공개 주소에 연결하시면 됩니다.

```
cargo build --locked --release
cargo test --locked --release
```

문제나 제안은 이 저장소의 Issue 로 남겨 주세요. 보안 문제는 공개 Issue 대신 [arete.ai.kr/contact](https://arete.ai.kr/contact) 로 알려 주세요.

## 라이선스

[MIT](LICENSE)

---

## English

**AI 뭐 쓰지? ("Which AI should I use?")** is a Korean directory of AI apps, agents, Skills and MCP servers, organised by task and field. This repository is the source of its **remote MCP server**, which lets AI assistants search the directory directly.

**Connect (no install):** `https://arete.ai.kr/connect/mcp` (Streamable HTTP)

- Claude Code: `claude mcp add --transport http arete https://arete.ai.kr/connect/mcp`

**Tools:** `search`, `find_by_field`, `get_ranking`, `get_trending`, `get_item`, `list_fields`. Answers include Korean-language support, free-plan status with the date it was checked, platforms, where to get it, and a first thing to try. Every answer carries attribution and a link.

**Limits:** 30/min and 300/day per IP without a key, or 120/min per key and 5,000/day per member with a free account key from [arete.ai.kr/connect](https://arete.ai.kr/connect).

**Privacy:** without a key, the client IP is kept in memory for rate limiting only (up to 24 h) and is never written to disk. With a key, only daily call counts per key are stored (deleted after 90 days). See the [privacy policy](https://arete.ai.kr/privacy).

The server calls the AI 뭐 쓰지? API for its data, so please use the public endpoint rather than self-hosting. MIT licensed.
