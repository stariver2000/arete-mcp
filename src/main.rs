//! arete-mcp-rs — 원격 MCP 서버(https://arete.ai.kr/connect/mcp)의 Rust 판. 파이썬 `arete-mcp --http` 와 같은 도구·같은 글·같은 한도를 낸다
//! (두 판의 답을 요청마다 견주는 시험으로 같음을 잰다).
//! 127.0.0.1 에만 연다: 앞에 X-Forwarded-For 를 넣는 역방향 프록시가 있다고 본다 (사용자 IP 한도가 그 머리를 믿는다).
//! 어디서 시작: main → http::router (HTTP 겉) → rpc (JSON-RPC) → tools (도구 몸통) → upstream (공개 API) · validate (인자 검사).
//! 환경 변수: ARETE_API_BASE (http:// 만, TLS 를 넣지 않았다), ARETE_INTERNAL_TOKEN (값은 어디에도 출력하지 않는다).

use std::sync::Arc;

use arete_mcp_rs::{assets, http, tools, upstream};

fn port_from_args() -> u16 {
    let args: Vec<String> = std::env::args().collect();
    match args.iter().position(|a| a == "--port") {
        Some(n) => args
            .get(n + 1)
            .and_then(|p| p.parse().ok())
            .unwrap_or_else(|| fail("--port 뒤에 번호")),
        None => fail("--port 가 없다"),
    }
}

fn fail(message: &str) -> ! {
    eprintln!("arete-mcp-rs: {message}");
    std::process::exit(2)
}

#[tokio::main]
async fn main() {
    let port = port_from_args();
    let base = std::env::var("ARETE_API_BASE").unwrap_or_else(|_| fail("ARETE_API_BASE 가 없다 (예: http://localhost:8000)"));
    if !base.starts_with("http://") {
        fail("ARETE_API_BASE 는 http:// 만 (TLS 없이 같은 기계의 API 를 부른다)");
    }
    let token = std::env::var("ARETE_INTERNAL_TOKEN").ok();
    assets::get(); // 자산을 띄울 때 한 번 읽어 틀리면 바로 죽게
    let tools = Arc::new(tools::Tools::new(upstream::Upstream::new(&base, token.as_deref())));
    // 계정 키의 쓴 양을 1분마다 마켓 API 로 (파이썬 UsageBuffer.start). 못 보내면 남겨 다음에 다시
    let flusher = tools.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(arete_mcp_rs::keys::FLUSH_EVERY);
        tick.tick().await;
        loop {
            tick.tick().await;
            let rows = flusher.usage.take();
            if !rows.is_empty()
                && !flusher
                    .upstream
                    .send_usage(&arete_mcp_rs::keys::UsageBuffer::rows_json(&rows))
                    .await
            {
                flusher.usage.put_back(rows);
            }
        }
    });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap_or_else(|e| fail(&format!("127.0.0.1:{port} 열기: {e}")));
    axum::serve(listener, http::router(tools))
        .await
        .unwrap_or_else(|e| fail(&format!("서버: {e}")));
}
