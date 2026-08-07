use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;
use warp::ws::Message;

use crate::messages::{BackendResponse, Environment};

#[derive(Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
    stream: bool,
}

#[derive(Deserialize)]
struct ChatCompletionChunk {
    choices: Vec<ChunkChoice>,
}

#[derive(Deserialize)]
struct ChunkChoice {
    delta: ChunkDelta,
}

#[derive(Deserialize)]
struct ChunkDelta {
    content: Option<String>,
    reasoning_content: Option<String>,
}

fn search_codebase(path: &std::path::Path, query: &str, results: &mut Vec<String>) {
    if results.len() >= 50 {
        return;
    } // Max 50 results
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().to_string();
            if file_name == "node_modules"
                || file_name == "target"
                || file_name == ".git"
                || file_name == "dist"
                || file_name == "build"
                || file_name == ".forge"
            {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                if meta.is_dir() {
                    search_codebase(&entry.path(), query, results);
                } else {
                    if let Ok(content) = std::fs::read_to_string(entry.path()) {
                        let mut line_num = 1;
                        for line in content.lines() {
                            if line.contains(query) {
                                // Extract relative path if possible, or just use file name for brevity
                                results.push(format!(
                                    "{}:{}: {}",
                                    entry.path().display(),
                                    line_num,
                                    line.trim()
                                ));
                                if results.len() >= 50 {
                                    return;
                                }
                            }
                            line_num += 1;
                        }
                    }
                }
            }
        }
    }
}

fn locate_file(path: &std::path::Path, filename_query: &str, results: &mut Vec<String>) {
    if results.len() >= 20 {
        return;
    } // Max 20 results
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().to_string();
            if file_name == "node_modules"
                || file_name == "target"
                || file_name == ".git"
                || file_name == ".forge"
            {
                continue;
            }
            if file_name.contains(filename_query) {
                results.push(entry.path().display().to_string());
            }
            if let Ok(meta) = entry.metadata() {
                if meta.is_dir() {
                    locate_file(&entry.path(), filename_query, results);
                }
            }
        }
    }
}

fn get_directory_tree(
    path: &str,
    prefix: String,
    max_depth: usize,
    current_depth: usize,
) -> String {
    if current_depth > max_depth {
        return String::new();
    }
    let mut tree = String::new();
    if let Ok(entries) = std::fs::read_dir(path) {
        let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
        entries.sort_by_key(|e| e.file_name());

        let mut filtered_entries = Vec::new();
        for entry in entries {
            let file_name = entry.file_name().to_string_lossy().to_string();
            if file_name == "node_modules"
                || file_name == "target"
                || file_name == ".git"
                || file_name == ".forge"
                || file_name == "dist"
                || file_name == "build"
            {
                continue;
            }
            filtered_entries.push(entry);
        }

        for (i, entry) in filtered_entries.iter().enumerate() {
            let is_last = i == filtered_entries.len() - 1;
            let file_name = entry.file_name().to_string_lossy().to_string();
            let marker = if is_last { "└── " } else { "├── " };
            tree.push_str(&format!("{}{}{}\n", prefix, marker, file_name));
            if let Ok(meta) = entry.metadata() {
                if meta.is_dir() {
                    let new_prefix = if is_last {
                        format!("{}    ", prefix)
                    } else {
                        format!("{}│   ", prefix)
                    };
                    tree.push_str(&get_directory_tree(
                        entry.path().to_str().unwrap_or(""),
                        new_prefix,
                        max_depth,
                        current_depth + 1,
                    ));
                }
            }
        }
    } else {
        tree.push_str(&format!("{}[Error reading directory]\n", prefix));
    }
    tree
}

pub async fn handle_ai_chat_stream(
    model: Value,
    profile: Option<Value>,
    chat_history: Vec<Value>,
    context: Value,
    prompt: String,
    request_id: Option<String>,
    client_sender: UnboundedSender<Message>,
    cancellation_token: CancellationToken,
) {
    let endpoint = model
        .get("endpoint")
        .and_then(|v| v.as_str())
        .unwrap_or("http://localhost:11434/v1");
    let model_identifier = model
        .get("identifier")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let temperature = model.get("temperature").and_then(|v| v.as_f64());
    let max_tokens = model
        .get("maxTokens")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);

    let context_window = model
        .get("contextWindow")
        .and_then(|v| v.as_u64())
        .unwrap_or(8192) as usize;

    // INCREASED DEFAULT: Reasoning models (Gemma-4, DeepSeek) need more room for <think> blocks.
    // If user didn't specify max_tokens, we default to 4096 instead of 2048.
    let default_max_tokens = 4096;
    let output_reserve = max_tokens.unwrap_or(default_max_tokens) as usize;
    let safety_margin = 500;

    // We will build the full system_text and prompt, then see how much budget is left for chat history.
    let mut stop_tokens: Vec<String> = vec![];

    if let Some(params) = model.get("customParams").and_then(|v| v.as_array()) {
        for param in params {
            if param.get("key").and_then(|v| v.as_str()) == Some("stop") {
                if let Some(val_str) = param.get("value").and_then(|v| v.as_str()) {
                    if let Ok(parsed_arr) = serde_json::from_str::<Vec<String>>(val_str) {
                        stop_tokens.extend(parsed_arr);
                    }
                }
            }
        }
    }

    // Thinking Budget Logic
    let mut reasoning_effort = None;
    if let Some(budget) = model.get("thinkingBudget").and_then(|v| v.as_str()) {
        if budget != "none" {
            reasoning_effort = Some(budget.to_string());
        }
    }

    let mut messages = Vec::new();

    // 1. Build System Prompt (Model rules + Profile rules)
    let mut system_text = String::new();

    // --- NEW: KNOWLEDGE BASE INTEGRATION (STRICT ROBOTIC PROTOCOL) ---
    // This is no longer a guideline; it is a hard system constraint.
    let is_advanced_mode = profile.as_ref().map_or(true, |p| {
        p.get("id").and_then(|v| v.as_str()).unwrap_or("") != "ask" // If not simple "ask" mode
    });

    let default_system_prompt = r#"You are an Elite AI Software Architect running NATIVELY INSIDE the user's local "Forge IDE".

CRITICAL PROTOCOL: YOU HAVE DIRECT, FULL ACCESS TO THE USER'S LOCAL FILESYSTEM AND TERMINAL.
You are an AUTONOMOUS AGENT. Do NOT ask the user to run commands, read files, or apply code unless strictly necessary. Do it yourself!

You have FOUR mandatory Sub-Agents. Output ONLY the exact tool tag to trigger them. The system will pause, execute the tool, and return the output to you.

1. CODE AGENT (Codebase Navigation):
   - Browse folders: @@CODE: tree <path>@@ (e.g. @@CODE: tree src@@)
   - Find file by name: @@CODE: locate <filename>@@ (e.g. @@CODE: locate main.rs@@)
   - Search inside files: @@CODE: search <text>@@ (e.g. @@CODE: search calculate_tax@@)
   - Read full file: @@CODE: read <path>@@ (e.g. @@CODE: read src/main.rs@@)
   - Read specific lines: @@CODE: read <path> | <start_line>-<end_line>@@ (e.g. @@CODE: read src/main.rs | 50-100@@)

2. RUN AGENT (Terminal Execution):
   - Execute shell commands, tests, or builds: @@RUN: <command>@@ (e.g. @@RUN: cargo check@@ or @@RUN: ls -al@@)
   > WARNING: For destructive commands (e.g. `rm -rf`), ALWAYS ask for permission first.

3. WEB AGENT (Internet Access):
   - Search the web: @@WEB: search <query>@@
   - Read a website: @@WEB: fetch <url> | <topic>@@

4. MEMORY AGENT (Knowledge Base):
   - Write markdown docs: @@MEMORY: write <filename> | <markdown_content>@@

HOW TO WRITE/EDIT CODE (CHANGE SETS):
When you want to modify the user's code, DO NOT use the MEMORY agent. Instead, just write the new code inside a markdown code block and put the FILE PATH as the very first line inside the block (as a comment). The IDE will automatically detect it and show an "Apply" button to the user.

Example Code Edit:
```rust
// src/utils.rs
fn calculate_tax(amount: f64) -> f64 {
    amount * 0.20
}
```

MANDATORY WORKFLOW:
- Use `locate` or `search` to find things quickly instead of guessing paths.
- Use `read` with line numbers for large files to save memory.
- If the user asks a question, gather context autonomously before answering."#;

    if is_advanced_mode {
        system_text.push_str(default_system_prompt);
    }

    if let Some(prof) = profile.as_ref() {
        if let Some(psp) = prof.get("systemPrompt").and_then(|v| v.as_str()) {
            if !system_text.is_empty() {
                system_text.push_str("\n\n--- CUSTOM PROFILE INSTRUCTIONS ---\n");
            }
            system_text.push_str(psp);
        }
    }

    let estimate_tokens = |s: &str| -> usize { (s.len() + 3) / 4 };

    // --- CONTEXT BUDGET CALCULATION ---
    let total_budget_for_alloc = context_window
        .saturating_sub(output_reserve)
        .saturating_sub(safety_margin);

    let mut file_budget_tokens: usize = 4096;
    let mut knowledge_budget_tokens: usize = 4096;

    if let Some(ctx_obj) = context.as_object() {
        if let Some(ctx_settings) = ctx_obj.get("contextSettings").and_then(|v| v.as_object()) {
            let strategy = ctx_settings
                .get("strategy")
                .and_then(|v| v.as_str())
                .unwrap_or("auto");

            if strategy == "custom" {
                let codebase_pct = ctx_settings
                    .get("customCodebaseBudget")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(70.0)
                    / 100.0;
                let tokens_total = (total_budget_for_alloc as f64 * codebase_pct) as usize;
                file_budget_tokens = tokens_total / 2;
                knowledge_budget_tokens = tokens_total / 2;
            } else if strategy == "prefer_codebase" {
                file_budget_tokens = (total_budget_for_alloc as f64 * 0.4) as usize;
                knowledge_budget_tokens = (total_budget_for_alloc as f64 * 0.4) as usize;
            } else if strategy == "prefer_history" {
                file_budget_tokens = (total_budget_for_alloc as f64 * 0.15) as usize;
                knowledge_budget_tokens = (total_budget_for_alloc as f64 * 0.15) as usize;
            } else {
                // "auto"
                file_budget_tokens = (total_budget_for_alloc as f64 * 0.3) as usize;
                knowledge_budget_tokens = (total_budget_for_alloc as f64 * 0.3) as usize;
            }
        }
    }

    println!("\n📊 [CONTEXT BUDGET MANAGER] Allocating Tokens:");
    println!("  -> Total Available Context: {}", context_window);
    println!("  -> Output Reserve: {}", output_reserve);
    println!("  -> Safety Margin: {}", safety_margin);
    println!("  -> Budget for Allocation: {}", total_budget_for_alloc);
    println!("  -> Codebase/File Budget: {} tokens", file_budget_tokens);
    println!("  -> Knowledge Budget: {} tokens", knowledge_budget_tokens);

    // Convert new user prompt to tokens
    let mut prompt_text_for_estimation = prompt.clone();

    // Note: Frontend sends `chatHistory` EXCLUDING the new user prompt.
    // We must manually append the new user prompt.
    let mut user_content = vec![json!({ "type": "text", "text": prompt })];

    if let Some(ctx_obj) = context.as_object() {
        if let Some(attachments) = ctx_obj.get("attachments").and_then(|v| v.as_array()) {
            for attachment in attachments {
                if let (Some(name), Some(content), Some(mime_type)) = (
                    attachment.get("name").and_then(|v| v.as_str()),
                    attachment.get("content").and_then(|v| v.as_str()),
                    attachment.get("type").and_then(|v| v.as_str()),
                ) {
                    if mime_type.starts_with("text/")
                        || content.starts_with('{')
                        || !content.starts_with("data:image/")
                    {
                        // Text attachment
                        let text_att = format!(
                            "\n\n=== ATTACHMENT: {} ===\n{}\n================\n",
                            name, content
                        );
                        prompt_text_for_estimation.push_str(&text_att);
                        user_content.push(json!({ "type": "text", "text": text_att }));
                    } else if mime_type.starts_with("image/") || content.starts_with("data:image/")
                    {
                        user_content.push(json!({
                            "type": "image_url",
                            "image_url": {
                                "url": content
                            }
                        }));
                        prompt_text_for_estimation.push_str(&" ".repeat(1000)); // ~250 tokens
                    }
                }
            }
        }
    }

    // Now append optional context conditionally based on FIXED budgets to preserve KV cache
    let mut context_text = String::new();

    // Include Active File (High Priority)
    if let Some(ctx_obj) = context.as_object() {
        // Prevent context poisoning in Ask mode: Do not send the active file if we are just chatting.
        let is_ask_mode = profile.as_ref().map_or(false, |p| {
            p.get("id").and_then(|v| v.as_str()) == Some("ask")
        });

        if !is_ask_mode {
            if let Some(active_file) = ctx_obj.get("activeFile").and_then(|v| v.as_object()) {
                if let (Some(path), Some(content)) = (
                    active_file.get("path").and_then(|v| v.as_str()),
                    active_file.get("content").and_then(|v| v.as_str()),
                ) {
                    let file_header = format!("\n\n=== ACTIVE FILE ({}) ===\n", path);
                    let file_footer = "\n================\n";
                    let header_tokens =
                        estimate_tokens(&file_header) + estimate_tokens(file_footer);

                    let allowed_file_budget = file_budget_tokens.saturating_sub(header_tokens);
                    let char_limit = allowed_file_budget * 4;
                    let truncated_content = if content.len() > char_limit {
                        println!(
                            "  ✂️ [TRUNCATION ALERT] Active file '{}' is too large ({} chars). Truncating to {} chars to protect RAM!",
                            path,
                            content.len(),
                            char_limit
                        );
                        format!(
                            "{}... [TRUNCATED DUE TO CONTEXT LIMIT]",
                            &content[..char_limit]
                        )
                    } else {
                        println!(
                            "  ✅ [BUDGET OK] Active file '{}' fits in context ({} chars).",
                            path,
                            content.len()
                        );
                        content.to_string()
                    };

                    context_text.push_str(&file_header);
                    context_text.push_str(&truncated_content);
                    context_text.push_str(file_footer);
                }
            }
        }
    }

    // Include Knowledge Base (Medium Priority)
    if let Some(ctx_obj) = context.as_object() {
        if let Some(knowledge_files) = ctx_obj.get("knowledgeFiles").and_then(|v| v.as_array()) {
            for k_file in knowledge_files {
                if let Some(filename) = k_file.as_str() {
                    if let Some(proj_root) = ctx_obj.get("projectRoot").and_then(|v| v.as_str()) {
                        let k_path = format!(
                            "{}/.forge/knowledge/{}",
                            proj_root.trim_end_matches('/'),
                            filename
                        );
                        if let Ok(k_content) = std::fs::read_to_string(&k_path) {
                            let k_header = format!("\n\n<knowledge_base topic=\"{}\">\n", filename);
                            let k_footer = "\n</knowledge_base>\n";
                            let overhead_tokens =
                                estimate_tokens(&k_header) + estimate_tokens(k_footer);

                            let allowed_k_budget =
                                knowledge_budget_tokens.saturating_sub(overhead_tokens);
                            let char_limit = allowed_k_budget * 4;
                            let truncated_k = if k_content.len() > char_limit {
                                format!(
                                    "{}... [KNOWLEDGE TRUNCATED DUE TO CONTEXT LIMIT]",
                                    &k_content[..char_limit]
                                )
                            } else {
                                k_content.clone()
                            };

                            context_text.push_str(&k_header);
                            context_text.push_str(&truncated_k);
                            context_text.push_str(k_footer);
                        }
                    }
                }
            }
        }
    }

    if !context_text.is_empty() {
        // We prepend the context to the current user prompt instead of the system prompt.
        // This ensures the System Prompt + History remains identical (KV CACHE HIT!)
        let mut new_prompt = String::new();
        new_prompt.push_str(&context_text);
        new_prompt.push_str("\n\n");
        new_prompt.push_str(&prompt);

        prompt_text_for_estimation = new_prompt.clone();

        // Update user_content array
        if let Some(first) = user_content.first_mut() {
            if first.get("type").and_then(|v| v.as_str()) == Some("text") {
                *first = json!({ "type": "text", "text": new_prompt });
            }
        }
    }

    let system_tokens = estimate_tokens(&system_text);
    let prompt_tokens = estimate_tokens(&prompt_text_for_estimation);

    // Remaining budget for chat history
    let mut remaining_history_budget = context_window
        .saturating_sub(output_reserve)
        .saturating_sub(safety_margin)
        .saturating_sub(system_tokens)
        .saturating_sub(prompt_tokens);

    let mut history_messages = Vec::new();

    // Iterate backwards to keep the most recent context
    for hist_msg in chat_history.into_iter().rev() {
        let role = hist_msg
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("user");
        let parts = hist_msg.get("parts").and_then(|v| v.as_array());

        let mut full_text = String::new();
        if let Some(parts_arr) = parts {
            for part in parts_arr {
                if part.get("type").and_then(|v| v.as_str()) == Some("text") {
                    if let Some(txt) = part.get("text").and_then(|v| v.as_str()) {
                        full_text.push_str(txt);
                    }
                }
            }
        }

        if !full_text.is_empty() {
            let msg_tokens = estimate_tokens(&full_text);
            if remaining_history_budget > msg_tokens + 50 {
                remaining_history_budget -= msg_tokens;
                history_messages.push(json!({ "role": role, "content": full_text }));
            } else if remaining_history_budget > 200 {
                // If it's a long message but we have *some* budget, truncate it
                let char_limit = remaining_history_budget * 4;
                let truncated = format!(
                    "...[TRUNCATED HISTORY] {}",
                    &full_text[full_text.len().saturating_sub(char_limit)..]
                );
                history_messages.push(json!({ "role": role, "content": truncated }));
                break; // Stop completely
            } else {
                break; // Stop adding older messages if we run out of budget
            }
        }
    }

    // Since we collected backwards, reverse it before appending
    history_messages.reverse();

    if !system_text.is_empty() {
        messages.push(json!({"role": "system", "content": system_text}));
    }

    messages.extend(history_messages);

    if user_content.len() == 1 {
        // Just text
        messages.push(json!({ "role": "user", "content": user_content[0].get("text").unwrap_or(&json!("")).as_str().unwrap_or("") }));
    } else {
        // Multi-modal array
        messages.push(json!({ "role": "user", "content": user_content }));
    }

    let mut loop_messages = messages.clone();
    let mut iteration = 0;

    loop {
        if iteration >= 15 {
            send_chunk(
                "\n\n**Error:** Max codebase iterations reached.".to_string(),
                true,
                &request_id,
                &client_sender,
            );
            break;
        }
        iteration += 1;

        // Dynamic Request Body parsing: check if customParams has mlx/ollama overrides
        let mut req_body_json = json!({
            "model": model_identifier,
            "messages": loop_messages,
            "stream": true,
        });

        if let Some(t) = temperature {
            req_body_json["temperature"] = json!(t);
        }
        if let Some(mt) = max_tokens {
            // Safety clamp: max_tokens should not exceed (context_window - safety_margin)
            let safety_max = context_window.saturating_sub(safety_margin) as u32;
            req_body_json["max_tokens"] = json!(mt.min(safety_max));
        } else {
            req_body_json["max_tokens"] = json!(default_max_tokens);
        }
        if !stop_tokens.is_empty() {
            req_body_json["stop"] = json!(stop_tokens);
        }
        if let Some(re) = reasoning_effort.clone() {
            req_body_json["reasoning_effort"] = json!(re);
        }

        // Inject any top-level custom parameters the user defined for this model
        if let Some(params) = model.get("customParams").and_then(|v| v.as_array()) {
            for param in params {
                let key = param.get("key").and_then(|v| v.as_str()).unwrap_or("");
                let val_str = param.get("value").and_then(|v| v.as_str()).unwrap_or("");

                // Skip "stop" as we already handled it
                if key == "stop" || key.is_empty() {
                    continue;
                }

                // Try to parse the value as JSON
                let parsed_val: Value = serde_json::from_str(val_str).unwrap_or(json!(val_str));
                req_body_json[key] = parsed_val;
            }
        }

        let client = Client::new();
        let url = format!("{}/chat/completions", endpoint.trim_end_matches('/'));

        let req_builder = client.post(&url).json(&req_body_json);

        // Add API key if present
        let req_builder = if let Some(api_key) = model.get("apiKey").and_then(|v| v.as_str()) {
            if !api_key.is_empty() {
                req_builder.bearer_auth(api_key)
            } else {
                req_builder
            }
        } else {
            req_builder
        };

        println!(
            "\n[DEBUG] === SENDING RAW HTTP REQUEST TO MODEL (Iteration {}) ===",
            iteration
        );
        println!("Endpoint: {}", url);

        match req_builder.send().await {
            Ok(res) => {
                let mut stream = res.bytes_stream();
                let mut buffer = String::new();
                let mut is_reasoning = false;
                let mut reasoning_field_used = false;
                let mut full_assistant_response = String::new();
                let mut code_command = None;

                while let Some(chunk_res) = stream.next().await {
                    if cancellation_token.is_cancelled() {
                        println!(
                            "Backend: AI stream cancelled for request {}",
                            request_id.as_deref().unwrap_or("unknown")
                        );
                        break;
                    }
                    if let Ok(chunk_bytes) = chunk_res {
                        let raw_chunk = String::from_utf8_lossy(&chunk_bytes);
                        buffer.push_str(&raw_chunk);

                        // Process complete lines from the buffer
                        while let Some(pos) = buffer.find('\n') {
                            let line = buffer[..pos].trim_end().to_string();
                            buffer = buffer[pos + 1..].to_string();

                            if line.starts_with("data: ") {
                                let data = &line[6..];
                                if data == "[DONE]" {
                                    continue;
                                }
                                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(data)
                                {
                                    if let Some(choices) =
                                        parsed.get("choices").and_then(|v| v.as_array())
                                    {
                                        if let Some(choice) = choices.first() {
                                            if let Some(delta) = choice.get("delta") {
                                                // Handle reasoning_content
                                                let reasoning_val = delta
                                                    .get("reasoning_content")
                                                    .or_else(|| delta.get("reasoning"));
                                                if let Some(reasoning) =
                                                    reasoning_val.and_then(|v| v.as_str())
                                                {
                                                    reasoning_field_used = true;
                                                    if !is_reasoning {
                                                        is_reasoning = true;
                                                        send_chunk(
                                                            format!("<think>\n{}", reasoning),
                                                            false,
                                                            &request_id,
                                                            &client_sender,
                                                        );
                                                    } else {
                                                        send_chunk(
                                                            reasoning.to_string(),
                                                            false,
                                                            &request_id,
                                                            &client_sender,
                                                        );
                                                    }
                                                }

                                                // Handle standard content
                                                if let Some(content) =
                                                    delta.get("content").and_then(|v| v.as_str())
                                                {
                                                    if is_reasoning
                                                        && reasoning_field_used
                                                        && !content.is_empty()
                                                    {
                                                        // Transitioning from reasoning field to content field
                                                        send_chunk(
                                                            "\n</think>\n".to_string(),
                                                            false,
                                                            &request_id,
                                                            &client_sender,
                                                        );
                                                        is_reasoning = false;
                                                        reasoning_field_used = false; // Reset
                                                    }

                                                    if content.contains("<think>")
                                                        || content.contains("<|think|>")
                                                    {
                                                        is_reasoning = true;
                                                    }

                                                    send_chunk(
                                                        content.to_string(),
                                                        false,
                                                        &request_id,
                                                        &client_sender,
                                                    );
                                                    full_assistant_response.push_str(content);

                                                    if content.contains("</think>")
                                                        || content.contains("</|think|>")
                                                    {
                                                        is_reasoning = false;
                                                    }

                                                    // Detect @@CODE: ... @@ or @@MEMORY: ... @@ mid-stream
                                                    if let Some(start_idx) =
                                                        full_assistant_response.rfind("@@MEMORY:")
                                                    {
                                                        let after_code = &full_assistant_response
                                                            [start_idx + 9..];
                                                        if let Some(end_idx) = after_code.find("@@")
                                                        {
                                                            let full_cmd =
                                                                after_code[..end_idx].trim();
                                                            code_command = Some((
                                                                "memory".to_string(),
                                                                full_cmd.to_string(),
                                                            ));
                                                        }
                                                    } else if let Some(start_idx) =
                                                        full_assistant_response.rfind("@@RUN:")
                                                    {
                                                        let after_code = &full_assistant_response
                                                            [start_idx + 6..];
                                                        if let Some(end_idx) = after_code.find("@@")
                                                        {
                                                            let full_cmd =
                                                                after_code[..end_idx].trim();
                                                            code_command = Some((
                                                                "run".to_string(),
                                                                full_cmd.to_string(),
                                                            ));
                                                        }
                                                    } else if let Some(start_idx) =
                                                        full_assistant_response.rfind("@@CODE:")
                                                    {
                                                        let after_code = &full_assistant_response
                                                            [start_idx + 7..];
                                                        if let Some(end_idx) = after_code.find("@@")
                                                        {
                                                            let full_cmd =
                                                                after_code[..end_idx].trim();
                                                            if let Some(space_idx) =
                                                                full_cmd.find(' ')
                                                            {
                                                                let cmd = full_cmd[..space_idx]
                                                                    .trim()
                                                                    .to_string();
                                                                let arg = full_cmd[space_idx..]
                                                                    .trim()
                                                                    .to_string();
                                                                code_command = Some((cmd, arg));
                                                            } else {
                                                                code_command = Some((
                                                                    full_cmd.to_string(),
                                                                    String::new(),
                                                                ));
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if code_command.is_some() {
                        // Break out of the stream immediately!
                        break;
                    }
                }

                if is_reasoning && code_command.is_none() {
                    send_chunk("</think>".to_string(), false, &request_id, &client_sender);
                }

                if let Some((cmd, arg)) = code_command {
                    // Send a status update to the frontend
                    let display_arg = if cmd == "memory" {
                        let short_arg = if arg.len() > 30 {
                            format!("{}...", &arg[..30])
                        } else {
                            arg.clone()
                        };
                        short_arg
                    } else {
                        arg.clone()
                    };

                    send_chunk(
                        format!(
                            "\n> ⚡ **Agent Triggered:** `{}` on `{}`\n\n",
                            cmd, display_arg
                        ),
                        false,
                        &request_id,
                        &client_sender,
                    );

                    // Execute filesystem action
                    let mut result_text = String::new();
                    let mut ui_feedback = String::new(); // What the user sees in the chat UI

                    let proj_root = context
                        .as_object()
                        .and_then(|o| o.get("projectRoot"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    if cmd == "memory" {
                        if arg.starts_with("write ") {
                            let rest = arg[6..].trim();
                            if let Some(pipe_idx) = rest.find('|') {
                                let filename = rest[..pipe_idx].trim();
                                let content = rest[pipe_idx + 1..].trim();

                                let path = format!(
                                    "{}/.forge/knowledge/{}",
                                    proj_root.trim_end_matches('/'),
                                    filename
                                );

                                // Ensure directory exists
                                if let Some(parent) = std::path::Path::new(&path).parent() {
                                    let _ = std::fs::create_dir_all(parent);
                                }

                                match std::fs::write(&path, content) {
                                    Ok(_) => {
                                        result_text =
                                            format!("Successfully wrote knowledge to {}", filename);
                                        ui_feedback = format!(
                                            "> 💾 **Saved to Knowledge Base:** `{}`\n",
                                            filename
                                        );
                                    }
                                    Err(e) => {
                                        result_text = format!(
                                            "Failed to write knowledge file {}: {}",
                                            filename, e
                                        );
                                        ui_feedback = format!("> ❌ **Failed to Save:** `{}`\n", e);
                                    }
                                }
                            } else {
                                result_text = "Error: Invalid syntax for @@MEMORY. Expected `@@MEMORY: write <file> | <content> @@`".to_string();
                                ui_feedback = format!("> ❌ **Syntax Error in Memory Agent**\n");
                            }
                        } else {
                            result_text =
                                "Error: Unknown memory command. Expected `write`".to_string();
                            ui_feedback = format!("> ❌ **Unknown Memory Command**\n");
                        }
                    } else if cmd == "search" {
                        let mut results = Vec::new();
                        search_codebase(std::path::Path::new(proj_root), &arg, &mut results);
                        if results.is_empty() {
                            result_text = format!("No results found for '{}'", arg);
                        } else {
                            result_text =
                                format!("Search results for '{}':\n{}", arg, results.join("\n"));
                        }
                        ui_feedback = format!(
                            "> 🔍 **Searched codebase for:** `{}` ({} results)\n\n",
                            arg,
                            results.len()
                        );
                    } else if cmd == "locate" {
                        let mut results = Vec::new();
                        locate_file(std::path::Path::new(proj_root), &arg, &mut results);
                        if results.is_empty() {
                            result_text = format!("No files found matching '{}'", arg);
                        } else {
                            result_text = format!("Files found:\n{}", results.join("\n"));
                        }
                        ui_feedback = format!(
                            "> 🧭 **Located file:** `{}` ({} results)\n\n",
                            arg,
                            results.len()
                        );
                    } else if cmd == "tree" {
                        let path = if arg.starts_with("/") {
                            arg.clone()
                        } else {
                            format!("{}/{}", proj_root.trim_end_matches('/'), arg)
                        };
                        let tree_str = get_directory_tree(&path, String::new(), 4, 0); // max depth 4
                        result_text = format!("Directory tree for {}:\n{}", path, tree_str);
                        ui_feedback = format!("```plaintext\n{}\n```\n", tree_str);
                    } else if cmd == "run" {
                        #[cfg(target_os = "windows")]
                        let (shell, shell_arg) = ("cmd", "/C");
                        #[cfg(not(target_os = "windows"))]
                        let (shell, shell_arg) = ("sh", "-c");

                        let mut command = tokio::process::Command::new(shell);
                        command.arg(shell_arg).arg(&arg);
                        if !proj_root.is_empty() {
                            command.current_dir(proj_root);
                        }

                        // Add a timeout of 15 seconds so long-running commands (e.g., servers) don't hang the loop
                        match tokio::time::timeout(
                            std::time::Duration::from_secs(15),
                            command.output(),
                        )
                        .await
                        {
                            Ok(Ok(output)) => {
                                let stdout = String::from_utf8_lossy(&output.stdout);
                                let stderr = String::from_utf8_lossy(&output.stderr);
                                let status = output.status;

                                let mut combined = format!("Exit Status: {}\n", status);
                                if !stdout.trim().is_empty() {
                                    combined.push_str(&format!("STDOUT:\n{}\n", stdout.trim()));
                                }
                                if !stderr.trim().is_empty() {
                                    combined.push_str(&format!("STDERR:\n{}\n", stderr.trim()));
                                }
                                if stdout.trim().is_empty() && stderr.trim().is_empty() {
                                    combined.push_str("(No output)\n");
                                }

                                let char_limit = 20000;
                                if combined.len() > char_limit {
                                    let trunc_msg = "\n... [OUTPUT TRUNCATED DUE TO LENGTH LIMIT]";
                                    let safe_len = char_limit - trunc_msg.len();
                                    result_text = format!("{}{}", &combined[..safe_len], trunc_msg);
                                } else {
                                    result_text = combined.clone();
                                }

                                // Limit UI output to ~1000 characters so we don't flood the chat screen
                                let ui_char_limit = 1000;
                                let ui_output = if combined.len() > ui_char_limit {
                                    format!(
                                        "{}... \n\n[OUTPUT TRUNCATED IN UI - AGENT SEES MORE]",
                                        &combined[..ui_char_limit]
                                    )
                                } else {
                                    combined.clone()
                                };
                                ui_feedback = format!("```bash\n$ {}\n{}\n```\n", arg, ui_output);
                            }
                            Ok(Err(e)) => {
                                result_text = format!("Failed to execute command '{}': {}", arg, e);
                                ui_feedback = format!("```bash\n$ {}\nError: {}\n```\n", arg, e);
                            }
                            Err(_) => {
                                result_text =
                                    format!("Command '{}' timed out after 15 seconds.", arg);
                                ui_feedback = format!(
                                    "```bash\n$ {}\n[TIMED OUT AFTER 15 SECONDS]\n```\n",
                                    arg
                                );
                            }
                        }
                    } else if cmd == "read" {
                        let mut actual_path = arg.clone();
                        let mut line_range: Option<(usize, usize)> = None;

                        if let Some(pipe_idx) = arg.find('|') {
                            actual_path = arg[..pipe_idx].trim().to_string();
                            let range_str = arg[pipe_idx + 1..].trim();
                            if let Some(dash_idx) = range_str.find('-') {
                                let start =
                                    range_str[..dash_idx].trim().parse::<usize>().unwrap_or(1);
                                let end = range_str[dash_idx + 1..]
                                    .trim()
                                    .parse::<usize>()
                                    .unwrap_or(usize::MAX);
                                line_range = Some((start, end));
                            }
                        }

                        let full_path = if actual_path.starts_with("/") {
                            actual_path.clone()
                        } else {
                            format!("{}/{}", proj_root.trim_end_matches('/'), actual_path)
                        };

                        match std::fs::read_to_string(&full_path) {
                            Ok(file_content) => {
                                if let Some((start, end)) = line_range {
                                    let lines: Vec<&str> = file_content.lines().collect();
                                    let start_idx = start.saturating_sub(1).min(lines.len());
                                    let end_idx = end.min(lines.len());
                                    let subset = lines[start_idx..end_idx].join("\n");
                                    result_text = format!(
                                        "File content ({}, lines {}-{}):\n{}",
                                        actual_path, start, end, subset
                                    );
                                    ui_feedback = format!(
                                        "> 📖 **Read File:** `{}` (lines {}-{})\n\n",
                                        actual_path, start, end
                                    );
                                } else {
                                    let char_limit = 40000;
                                    if file_content.len() > char_limit {
                                        result_text = format!(
                                            "File content ({}):\n{}... [TRUNCATED]",
                                            actual_path,
                                            &file_content[..char_limit]
                                        );
                                    } else {
                                        result_text = format!(
                                            "File content ({}):\n{}",
                                            actual_path, file_content
                                        );
                                    }
                                    ui_feedback = format!(
                                        "> 📖 **Read File:** `{}` *(Loaded into context)*\n\n",
                                        actual_path
                                    );
                                }
                            }
                            Err(e) => {
                                result_text = format!("Failed to read file {}: {}", actual_path, e);
                                ui_feedback =
                                    format!("> ❌ **Failed to Read:** `{}` ({})\n", actual_path, e);
                            }
                        }
                    } else {
                        result_text = format!("Unknown command: {}", cmd);
                        ui_feedback = format!("> ❓ **Unknown command:** `{}`\n", cmd);
                    }

                    // Send the UI Feedback immediately to the chat window
                    send_chunk(ui_feedback, false, &request_id, &client_sender);

                    // Append the accumulated assistant response so far
                    loop_messages
                        .push(json!({"role": "assistant", "content": full_assistant_response}));

                    // Append the system execution result
                    loop_messages.push(json!({"role": "system", "content": format!("Tool Result:\n{}", result_text)}));

                    // Continue to the next iteration to query the LLM again
                    continue;
                }

                // If no tool was called, we are done
                send_chunk(String::new(), true, &request_id, &client_sender);
                break;
            }
            Err(e) => {
                send_chunk(
                    format!("\n\n**Error:** Failed to connect to AI provider: {}", e),
                    true,
                    &request_id,
                    &client_sender,
                );
                break;
            }
        }
    }
}

fn send_chunk(
    chunk: String,
    done: bool,
    request_id: &Option<String>,
    sender: &UnboundedSender<Message>,
) {
    let msg_id = request_id.clone().unwrap_or_else(|| "mock_msg".to_string());
    let resp = BackendResponse::AiChatStreamResponse {
        message_id: msg_id,
        chunk: if chunk.is_empty() && done {
            None
        } else {
            Some(chunk)
        },
        done,
        request_id: request_id.clone(),
    };
    if let Ok(json_str) = serde_json::to_string(&resp) {
        let _ = sender.send(Message::text(json_str));
    }
}
