use worker::*;
use worker_macros::event;
use serde::{Deserialize, Serialize};
use html5ever::parse_document;
use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};

#[derive(Debug, Deserialize)]
struct ConvertRequest {
    url: String,
    #[serde(default)]
    include_links: bool,
}

#[derive(Debug, Serialize)]
struct ConvertResponse {
    markdown: String,
    stats: ConversionStats,
}

#[derive(Debug, Serialize)]
struct ConversionStats {
    original_size: usize,
    converted_size: usize,
}

fn html_to_markdown(html: &str, include_links: bool) -> String {
    let dom = parse_document(RcDom::default(), Default::default())
        .from_utf8()
        .read_from(&mut html.as_bytes())
        .unwrap();

    let mut markdown = String::new();
    process_node(&dom.document, &mut markdown, include_links);
    markdown.trim().to_string()
}

fn process_node(handle: &Handle, markdown: &mut String, include_links: bool) {
    let node = &handle.data;

    match node {
        NodeData::Element { ref name, ref attrs, .. } => {
            let tag_name = name.local.as_ref();

            match tag_name {
                "p" => {
                    markdown.push_str("\n\n");
                    process_children(handle, markdown, include_links);
                    markdown.push('\n');
                },
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    let level = tag_name[1..].parse::<usize>().unwrap();
                    markdown.push_str("\n");
                    markdown.push_str(&"#".repeat(level));
                    markdown.push(' ');
                    process_children(handle, markdown, include_links);
                    markdown.push('\n');
                },
                "a" if include_links => {
                    let href = attrs.borrow()
                        .iter()
                        .find(|attr| attr.name.local.as_ref() == "href")
                        .map(|attr| attr.value.to_string());

                    if let Some(href) = href {
                        markdown.push('[');
                        process_children(handle, markdown, include_links);
                        markdown.push_str("](");
                        markdown.push_str(&href);
                        markdown.push(')');
                    } else {
                        process_children(handle, markdown, include_links);
                    }
                },
                "strong" | "b" => {
                    markdown.push_str("**");
                    process_children(handle, markdown, include_links);
                    markdown.push_str("**");
                },
                "em" | "i" => {
                    markdown.push('*');
                    process_children(handle, markdown, include_links);
                    markdown.push('*');
                },
                "code" => {
                    markdown.push('`');
                    process_children(handle, markdown, include_links);
                    markdown.push('`');
                },
                "pre" => {
                    markdown.push_str("\n```\n");
                    process_children(handle, markdown, include_links);
                    markdown.push_str("\n```\n");
                },
                "ul" => {
                    markdown.push('\n');
                    process_children(handle, markdown, include_links);
                    markdown.push('\n');
                },
                "ol" => {
                    markdown.push('\n');
                    process_children(handle, markdown, include_links);
                    markdown.push('\n');
                },
                "li" => {
                    markdown.push_str("* ");
                    process_children(handle, markdown, include_links);
                    markdown.push('\n');
                },
                _ => process_children(handle, markdown, include_links),
            }
        },
        NodeData::Text { ref contents } => {
            let text = contents.borrow().to_string();
            if !text.trim().is_empty() {
                markdown.push_str(&text.trim());
            }
        },
        _ => process_children(handle, markdown, include_links),
    }
}

fn process_children(handle: &Handle, markdown: &mut String, include_links: bool) {
    for child in handle.children.borrow().iter() {
        process_node(child, markdown, include_links);
    }
}

#[event(fetch)]
pub async fn main(mut req: Request, _env: Env, _ctx: Context) -> worker::Result<Response> {
    console_error_panic_hook::set_once();

    if req.method() == Method::Options {
        let mut response = Response::empty()?;
        response = response.with_cors(&Cors::default())?;
        return Ok(response.with_status(204));
    }

    if req.method() != Method::Post {
        return Response::error("Method Not Allowed", 405);
    }

    let request: ConvertRequest = match req.json().await {
        Ok(req) => req,
        Err(_) => return Response::error("Invalid JSON body", 400),
    };

    let mut response = match Fetch::Url(request.url.parse().unwrap()).send().await {
        Ok(resp) => resp,
        Err(_) => return Response::error("Failed to fetch URL", 400),
    };

    let html = match response.text().await {
        Ok(text) => text,
        Err(_) => return Response::error("Failed to read response", 500),
    };

    let original_size = html.len();

    let markdown = html_to_markdown(&html, request.include_links);

    let response_data = ConvertResponse {
        markdown: markdown.clone(),
        stats: ConversionStats {
            original_size,
            converted_size: markdown.len(),
        },
    };

    let mut response = Response::from_json(&response_data)?;
    response = response.with_cors(&Cors::default())?;
    Ok(response.with_status(200))
}