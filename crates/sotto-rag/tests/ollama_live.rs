//! Manual checks against the real local Ollama. They are ignored by default:
//!
//! ```text
//! cargo test -p sotto-rag --test ollama_live -- --ignored --nocapture
//! SOTTO_EMBED_MODEL=bge-m3 cargo test -p sotto-rag --test ollama_live -- --ignored --nocapture
//! ```
//!
//! The model override builds a pin from whatever digest the server reports, so it measures a
//! candidate without editing `config/embedding.toml`.

use std::time::Instant;

use sotto_rag::{EmbedKind, Embedder, OllamaEmbedder, PinnedModel, cosine};

const PASSAGES: [(&str, &str); 12] = [
    (
        "reembolso",
        "Política de reembolso: o cliente pode solicitar o reembolso integral em até 30 dias após a contratação do plano anual, sem justificativa.",
    ),
    (
        "refund",
        "Refund policy: customers can request a full refund within 30 days of buying the annual plan, no questions asked.",
    ),
    (
        "seguranca",
        "Segurança: todos os dados são criptografados em repouso com AES-256 e em trânsito com TLS 1.3. Os backups são mantidos por 35 dias.",
    ),
    (
        "sla",
        "Service levels: the platform commits to 99.9% monthly uptime. Credits of 10% of the monthly fee apply for each 0.1% below the target.",
    ),
    (
        "precos",
        "Preços: o plano Essencial custa R$ 49 por usuário ao mês, e o plano Equipe custa R$ 89 por usuário ao mês com cobrança anual.",
    ),
    (
        "onboarding",
        "Onboarding takes about two weeks: a kickoff call, data import, two training sessions and a go-live checklist reviewed with your account manager.",
    ),
    (
        "integracoes",
        "Integrações disponíveis: Salesforce, HubSpot, Slack e Google Calendar. A API REST permite criar integrações próprias com autenticação OAuth 2.0.",
    ),
    (
        "support",
        "Support hours are Monday to Friday, 9am to 6pm Brasília time. Enterprise customers get a dedicated Slack channel and a four-hour response target.",
    ),
    (
        "lgpd",
        "Conformidade com a LGPD: o encarregado de dados atende pelo e-mail privacidade@exemplo.com e os pedidos dos titulares são respondidos em até 15 dias.",
    ),
    (
        "roadmap",
        "Roadmap for next quarter: offline mode, a mobile app for iOS and Android, and single sign-on with SAML for larger teams.",
    ),
    (
        "cancelamento",
        "Para cancelar a assinatura, o administrador acessa Configurações, depois Cobrança, e escolhe Cancelar plano. O acesso continua até o fim do período pago.",
    ),
    (
        "limites",
        "Plan limits: the Essential plan includes 5 GB of storage per user and up to 10,000 API calls per day; the Team plan raises both limits tenfold.",
    ),
];

const QUERIES: [(&str, &str, &str); 12] = [
    (
        "pt",
        "Posso pedir meu dinheiro de volta depois de assinar?",
        "reembolso",
    ),
    ("en", "How long do I have to get my money back?", "refund"),
    (
        "pt",
        "Como os dados dos clientes são protegidos?",
        "seguranca",
    ),
    (
        "en",
        "What uptime do you guarantee and what happens if you miss it?",
        "sla",
    ),
    ("pt", "Quanto custa por pessoa?", "precos"),
    ("en", "How long does it take to get started?", "onboarding"),
    ("pt", "Vocês integram com o Slack?", "integracoes"),
    ("en", "When can I call support?", "support"),
    (
        "pt",
        "Quem é o responsável pela proteção de dados pessoais?",
        "lgpd",
    ),
    ("en", "Are you planning single sign-on?", "roadmap"),
    ("pt", "Como faço para encerrar o plano?", "cancelamento"),
    (
        "en",
        "How much storage do I get on the cheaper plan?",
        "limites",
    ),
];

fn prefixes(model: &str) -> (&'static str, &'static str) {
    if model.starts_with("embeddinggemma") {
        ("task: search result | query: ", "title: none | text: ")
    } else if model.starts_with("nomic") {
        ("search_query: ", "search_document: ")
    } else {
        ("", "")
    }
}

type Res<T> = Result<T, Box<dyn std::error::Error>>;

async fn embedder() -> Res<OllamaEmbedder> {
    let Ok(model) = std::env::var("SOTTO_EMBED_MODEL") else {
        return Ok(OllamaEmbedder::bundled()?);
    };
    let tags: serde_json::Value = reqwest::get("http://127.0.0.1:11434/api/tags")
        .await?
        .json()
        .await?;
    let digest = tags["models"]
        .as_array()
        .and_then(|m| {
            m.iter()
                .find(|e| e["name"].as_str().is_some_and(|n| n.starts_with(&model)))
        })
        .and_then(|e| e["digest"].as_str())
        .ok_or("model not installed")?
        .to_string();
    let probe = PinnedModel {
        name: model.clone(),
        digest,
        dimensions: 1,
        query_prefix: String::new(),
        document_prefix: String::new(),
        as_of: String::new(),
    };
    // Dimensions are unknown until the first answer; the error carries them.
    let dims = match OllamaEmbedder::new(probe.clone())?
        .embed(EmbedKind::Query, &["probe".into()])
        .await
    {
        Err(sotto_rag::EmbedError::Dimensions { found, .. }) => found,
        other => return Err(format!("unexpected probe result: {other:?}").into()),
    };
    let (q, d) = prefixes(&model);
    Ok(OllamaEmbedder::new(PinnedModel {
        dimensions: dims,
        query_prefix: q.into(),
        document_prefix: d.into(),
        ..probe
    })?)
}

#[tokio::test]
#[ignore = "needs a local Ollama with the embedding model installed"]
async fn retrieval_sanity_pt_br_and_english() -> Res<()> {
    let e = embedder().await?;
    eprintln!(
        "model: {} ({} dims)",
        e.pinned().name,
        e.pinned().dimensions
    );
    e.verify().await?;

    // Cold start: includes loading the model into memory.
    let t = Instant::now();
    let _ = e.embed(EmbedKind::Query, &["warm up".into()]).await?;
    eprintln!("first call (cold load): {} ms", t.elapsed().as_millis());

    let docs: Vec<String> = PASSAGES.iter().map(|(_, t)| (*t).to_string()).collect();
    let t = Instant::now();
    let doc_vecs = e.embed(EmbedKind::Document, &docs).await?;
    eprintln!(
        "{} passages in one call: {} ms",
        docs.len(),
        t.elapsed().as_millis()
    );

    // Indexing speed on window-sized chunks (about 1,200 bytes, the 320-token window).
    let long: Vec<String> = (0..64)
        .map(|i| {
            let mut t = format!("Section {i}. ");
            while t.len() < 1_200 {
                t.push_str(PASSAGES[i % PASSAGES.len()].1);
                t.push(' ');
            }
            t
        })
        .collect();
    let t = Instant::now();
    for batch in long.chunks(16) {
        e.embed(EmbedKind::Document, batch).await?;
    }
    let ms = t.elapsed().as_millis().max(1);
    eprintln!(
        "64 chunks of ~1200 bytes in batches of 16: {ms} ms ({} chunks/s)",
        64_000 / ms
    );

    let mut top1 = 0;
    let mut top3 = 0;
    let mut times = Vec::new();
    for (lang, q, expected) in QUERIES {
        let t = Instant::now();
        let qv = e.embed(EmbedKind::Query, &[q.to_string()]).await?.remove(0);
        times.push(t.elapsed().as_millis());
        let mut ranked: Vec<(&str, f32)> = PASSAGES
            .iter()
            .zip(&doc_vecs)
            .map(|((id, _), v)| (*id, cosine(&qv, v).unwrap_or(f32::MIN)))
            .collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
        let pos = ranked
            .iter()
            .position(|(id, _)| *id == expected)
            .ok_or("expected passage missing")?;
        top1 += usize::from(pos == 0);
        top3 += usize::from(pos < 3);
        eprintln!("[{lang}] rank {} score {:.3} | {q}", pos + 1, ranked[pos].1);
    }
    // Unrelated questions should score below the core's retrieval floor (0.2).
    for q in [
        "What is the weather forecast for tomorrow in Lisbon?",
        "Qual é a receita de bolo de cenoura?",
        "Who won the football match last night?",
    ] {
        let qv = e.embed(EmbedKind::Query, &[q.to_string()]).await?.remove(0);
        let best = doc_vecs
            .iter()
            .filter_map(|v| cosine(&qv, v))
            .fold(f32::MIN, f32::max);
        eprintln!("[unrelated] best score {best:.3} | {q}");
    }
    times.sort_unstable();
    eprintln!(
        "top-1 {top1}/{n}, top-3 {top3}/{n}; query latency warm: median {} ms, max {} ms",
        times[times.len() / 2],
        times[times.len() - 1],
        n = QUERIES.len()
    );
    assert!(top3 >= 10, "top-3 recall too low: {top3}/12");
    Ok(())
}
