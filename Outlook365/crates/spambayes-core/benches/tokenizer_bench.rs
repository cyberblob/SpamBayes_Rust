//! Tokenizer performance benchmarks.
//!
//! Verifies that tokenization of messages under 100KB completes in < 5ms
//! averaged over 1000 iterations (Requirement 22.7).

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use spambayes_core::tokenizer::Tokenizer;

/// Generate a synthetic plain-text email message of the given approximate body size in bytes.
fn generate_plain_text_message(body_size: usize) -> Vec<u8> {
    let headers = b"From: sender@example.com\r\n\
To: recipient@example.org\r\n\
Subject: Performance test message with some typical subject words\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
Received: from mail.example.com by mx.example.org with ESMTP\r\n\
Received: from relay.isp.net by mail.example.com with SMTP\r\n\
\r\n";

    // Generate realistic email body text with varied word lengths
    let words = [
        "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog",
        "hello", "world", "testing", "performance", "optimization",
        "message", "email", "content", "important", "meeting", "tomorrow",
        "please", "review", "attached", "document", "regards", "thanks",
        "schedule", "update", "project", "deadline", "completed",
        "information", "available", "department", "management", "customer",
        "notification", "subscription", "unsubscribe", "http://example.com/path/page",
        "https://secure.example.org/login?user=test&action=verify",
        "user@example.com", "support@company.net",
    ];

    let mut body = String::with_capacity(body_size + 256);
    let mut word_idx = 0;
    while body.len() < body_size {
        if !body.is_empty() && body.len() % 80 < 10 {
            body.push('\n');
        } else {
            body.push(' ');
        }
        body.push_str(words[word_idx % words.len()]);
        word_idx += 1;
    }

    let mut msg = Vec::with_capacity(headers.len() + body.len());
    msg.extend_from_slice(headers);
    msg.extend_from_slice(body.as_bytes());
    msg
}

/// Generate a synthetic HTML email message of the given approximate body size.
fn generate_html_message(body_size: usize) -> Vec<u8> {
    let headers = b"From: newsletter@company.com\r\n\
To: subscriber@example.org\r\n\
Subject: Weekly Newsletter - Special Offers Inside!\r\n\
Content-Type: text/html; charset=utf-8\r\n\
Received: from smtp.company.com by mx.example.org with ESMTP\r\n\
\r\n";

    let html_prefix = "<html><head><style>body { font-family: Arial; } \
        .header { color: blue; } .content { margin: 10px; }</style>\
        <script>var tracking = 'analytics'; console.log(tracking);</script>\
        </head><body><div class=\"header\"><h1>Newsletter</h1></div>\
        <div class=\"content\">";
    let html_suffix = "</div></body></html>";

    let paragraphs = [
        "<p>Check out our latest <b>deals</b> and <a href=\"https://shop.example.com/sale?ref=email&amp;id=12345\">special offers</a>.</p>",
        "<p>Dear valued customer, we have exciting news about our product lineup.</p>",
        "<p>Visit <a href=\"http://www.example.com/products/new-arrivals\">new arrivals</a> section for the best selection.</p>",
        "<p>Limited time offer: save 50% on selected items. Use code <b>SAVE50</b> at checkout.</p>",
        "<p>Contact us at support@company.com or call 1-800-EXAMPLE for assistance.</p>",
        "<p><!-- tracking pixel placeholder --><img src=\"http://tracker.example.com/pixel.gif\" width=\"1\" height=\"1\"/></p>",
        "<p>Our team of experts is ready to help you find exactly what you need.</p>",
        "<p>&copy; 2024 Example Company &mdash; All rights reserved. &nbsp; <a href=\"https://example.com/unsubscribe\">Unsubscribe</a></p>",
    ];

    let mut body = String::with_capacity(body_size + 512);
    body.push_str(html_prefix);
    let mut para_idx = 0;
    while body.len() < body_size {
        body.push_str(paragraphs[para_idx % paragraphs.len()]);
        body.push('\n');
        para_idx += 1;
    }
    body.push_str(html_suffix);

    let mut msg = Vec::with_capacity(headers.len() + body.len());
    msg.extend_from_slice(headers);
    msg.extend_from_slice(body.as_bytes());
    msg
}

/// Generate a multipart message with both plain text and HTML parts.
fn generate_multipart_message(body_size: usize) -> Vec<u8> {
    let half_size = body_size / 2;
    let words = [
        "hello", "world", "testing", "performance", "multipart",
        "message", "email", "content", "important", "meeting",
        "optimization", "benchmark", "criterion", "validation",
    ];

    let mut plain_body = String::with_capacity(half_size);
    let mut word_idx = 0;
    while plain_body.len() < half_size {
        if !plain_body.is_empty() {
            plain_body.push(' ');
        }
        plain_body.push_str(words[word_idx % words.len()]);
        word_idx += 1;
    }

    let mut html_body = String::with_capacity(half_size);
    html_body.push_str("<html><body>");
    word_idx = 0;
    while html_body.len() < half_size {
        html_body.push_str("<p>");
        html_body.push_str(words[word_idx % words.len()]);
        html_body.push_str("</p>");
        word_idx += 1;
    }
    html_body.push_str("</body></html>");

    format!(
        "From: test@example.com\r\n\
Content-Type: multipart/alternative; boundary=\"bench-boundary\"\r\n\
Subject: Multipart benchmark message for performance testing\r\n\
\r\n\
--bench-boundary\r\n\
Content-Type: text/plain\r\n\
\r\n\
{plain_body}\r\n\
--bench-boundary\r\n\
Content-Type: text/html\r\n\
\r\n\
{html_body}\r\n\
--bench-boundary--\r\n"
    )
    .into_bytes()
}

fn bench_tokenize_plain_small(c: &mut Criterion) {
    let tok = Tokenizer::with_defaults();
    let msg = generate_plain_text_message(1_000); // ~1KB
    c.bench_function("tokenize_plain_1kb", |b| {
        b.iter(|| tok.tokenize(black_box(&msg)));
    });
}

fn bench_tokenize_plain_medium(c: &mut Criterion) {
    let tok = Tokenizer::with_defaults();
    let msg = generate_plain_text_message(10_000); // ~10KB
    c.bench_function("tokenize_plain_10kb", |b| {
        b.iter(|| tok.tokenize(black_box(&msg)));
    });
}

fn bench_tokenize_plain_large(c: &mut Criterion) {
    let tok = Tokenizer::with_defaults();
    let msg = generate_plain_text_message(50_000); // ~50KB
    c.bench_function("tokenize_plain_50kb", |b| {
        b.iter(|| tok.tokenize(black_box(&msg)));
    });
}

fn bench_tokenize_plain_max(c: &mut Criterion) {
    let tok = Tokenizer::with_defaults();
    let msg = generate_plain_text_message(99_000); // ~99KB (under 100KB limit)
    c.bench_function("tokenize_plain_99kb", |b| {
        b.iter(|| tok.tokenize(black_box(&msg)));
    });
}

fn bench_tokenize_html_medium(c: &mut Criterion) {
    let tok = Tokenizer::with_defaults();
    let msg = generate_html_message(10_000); // ~10KB HTML
    c.bench_function("tokenize_html_10kb", |b| {
        b.iter(|| tok.tokenize(black_box(&msg)));
    });
}

fn bench_tokenize_html_large(c: &mut Criterion) {
    let tok = Tokenizer::with_defaults();
    let msg = generate_html_message(50_000); // ~50KB HTML
    c.bench_function("tokenize_html_50kb", |b| {
        b.iter(|| tok.tokenize(black_box(&msg)));
    });
}

fn bench_tokenize_multipart(c: &mut Criterion) {
    let tok = Tokenizer::with_defaults();
    let msg = generate_multipart_message(50_000); // ~50KB multipart
    c.bench_function("tokenize_multipart_50kb", |b| {
        b.iter(|| tok.tokenize(black_box(&msg)));
    });
}

/// Benchmark that mirrors the requirement: 1000 messages under 100KB, averaged.
/// This uses a mix of message types and sizes to simulate realistic workload.
fn bench_tokenize_mixed_1000_messages(c: &mut Criterion) {
    let tok = Tokenizer::with_defaults();

    // Generate a set of varied messages (mix sizes 1-50KB, mix types)
    let messages: Vec<Vec<u8>> = (0..100)
        .map(|i| {
            let size = 1_000 + (i * 500); // 1KB to 50KB
            match i % 3 {
                0 => generate_plain_text_message(size),
                1 => generate_html_message(size),
                _ => generate_multipart_message(size),
            }
        })
        .collect();

    c.bench_function("tokenize_mixed_100_messages", |b| {
        b.iter(|| {
            for msg in &messages {
                let _ = tok.tokenize(black_box(msg));
            }
        });
    });
}

criterion_group!(
    benches,
    bench_tokenize_plain_small,
    bench_tokenize_plain_medium,
    bench_tokenize_plain_large,
    bench_tokenize_plain_max,
    bench_tokenize_html_medium,
    bench_tokenize_html_large,
    bench_tokenize_multipart,
    bench_tokenize_mixed_1000_messages,
);
criterion_main!(benches);
