use std::time::Duration;

use super::*;

fn echo_catalog() -> Arc<Catalog> {
    Arc::new(Catalog::new(vec![ToolSpec {
        name: "echo".into(),
        description: "Echoes text. More words.".into(),
        parameters: json!({"type": "object", "properties": {"text": {"type": "string"}}}),
    }]))
}

fn soon() -> Interrupt {
    Interrupt::until(Instant::now() + Duration::from_secs(30))
}

/// Answers `echo` as a host would and refuses every other tool.
fn echo_host() -> (mpsc::Sender<Request>, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = mpsc::channel::<Request>(8);
    let answering = tokio::spawn(async move {
        while let Some(request) = rx.recv().await {
            let args: Value = serde_json::from_str(&request.arguments).unwrap();
            let reply = match request.name.as_str() {
                "echo" => Reply::Done(format!("echo:{}", args["text"].as_str().unwrap_or(""))),
                other => Reply::Failed(format!("not permitted: {other}")),
            };
            let _ = request.reply.send(reply);
        }
    });
    (tx, answering)
}

async fn with(script: Script, kept: Kept, limits: Limits, interrupt: Interrupt) -> Ended {
    let (tx, answering) = echo_host();
    let ended = run(script, echo_catalog(), kept, limits, interrupt, tx).await;
    answering.abort();
    ended
}

async fn js(code: &str) -> Ended {
    with(
        Script::javascript(code),
        Kept::default(),
        Limits::default(),
        soon(),
    )
    .await
}

#[tokio::test]
async fn tools_are_awaited_in_parallel_and_only_the_output_comes_back() {
    let ended = js(
        r#"const [a, b] = await Promise.all([tools.echo({text: "x"}), tools.echo({text: "y"})]);
           console.log("got", a);
           text(b);
           return {n: 2};"#,
    )
    .await;
    assert!(ended.ok(), "{}", ended.text);
    assert_eq!(ended.text, "got echo:x\necho:y\n{\n  \"n\": 2\n}");
    assert_eq!(ended.calls, 2);
    assert!(!ended.cut);
}

#[tokio::test]
async fn a_refused_call_rejects_and_a_failed_script_keeps_its_partial_output() {
    let ended = js(r#"text("before");
           const settled = await Promise.allSettled([tools.nope({})]);
           text(settled[0].status, settled[0].reason.message);
           throw new Error("boom");"#)
    .await;
    assert_eq!(ended.ending, Ending::Failed);
    assert!(
        ended
            .text
            .starts_with("before\nrejected there is no tool nope for scripts"),
        "{}",
        ended.text
    );
    assert!(
        ended.text.contains("Script error: Error: boom"),
        "{}",
        ended.text
    );
}

#[tokio::test]
async fn the_sandbox_has_no_way_out_but_the_tools() {
    let ended =
        js("return [typeof fetch, typeof require, typeof setTimeout, typeof process, ALL_TOOLS.length].join(',')")
            .await;
    assert_eq!(ended.text, "undefined,undefined,undefined,undefined,1");
    assert_eq!(
        js("return searchTools('ECHO text')[0].description")
            .await
            .text,
        "Echoes text."
    );
    assert_eq!(
        js("return describeTool('echo').parameters.type").await.text,
        "object"
    );
}

#[tokio::test]
async fn regular_expressions_are_not_there_but_the_string_methods_are() {
    assert!(!js("return /a/.test('a')").await.ok());
    let constructed = js("return new RegExp('a').test('a')").await;
    assert!(
        !constructed.ok() && constructed.text.contains("RegExp"),
        "{}",
        constructed.text
    );
    let strings = js("return ['a,b'.split(',').length, 'xay'.includes('a'), 'a-b'.replaceAll('-', '+')].join(' ')").await;
    assert_eq!(strings.text, "2 true a+b");
}

#[tokio::test]
async fn store_keeps_values_only_from_scripts_that_succeed() {
    let kept = Kept::default();
    let run = |code: &str, kept: &Kept| {
        with(
            Script::javascript(code),
            Arc::clone(kept),
            Limits::default(),
            soon(),
        )
    };
    assert!(
        run("store('a', {v: 1}); return load('a').v", &kept)
            .await
            .ok()
    );
    assert!(
        !run("store('b', 2); throw new Error('no')", &kept)
            .await
            .ok()
    );
    assert_eq!(
        run("return [load('a').v, load('b')].join(',')", &kept)
            .await
            .text,
        "1,"
    );
}

#[tokio::test]
async fn a_busy_loop_ends_at_the_deadline() {
    let deadline = Interrupt::until(Instant::now() + Duration::from_millis(200));
    let ended = with(
        Script::javascript("text('a'); while (true) {}"),
        Kept::default(),
        Limits::default(),
        deadline,
    )
    .await;
    assert_eq!(ended.ending, Ending::Interrupted, "{}", ended.text);
    assert_eq!(ended.text, "a\nScript interrupted: its time ran out.");
}

#[tokio::test]
async fn the_hosts_stop_ends_a_script_waiting_for_a_call() {
    // A host that takes the call and never answers: only the stop can end it.
    let (tx, mut rx) = mpsc::channel::<Request>(8);
    let holding = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Some(request) = rx.recv().await {
            held.push(request);
        }
    });
    let interrupt = soon();
    let stop = interrupt.stopper();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        stop.store(true, Ordering::SeqCst);
    });
    let started = Instant::now();
    let ended = run(
        Script::javascript("await tools.echo({text: 'x'}); return 'never'"),
        echo_catalog(),
        Kept::default(),
        Limits::default(),
        interrupt,
        tx,
    )
    .await;
    holding.abort();
    assert_eq!(ended.ending, Ending::Interrupted, "{}", ended.text);
    assert_eq!(ended.text, "Script stopped.");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn a_nudge_cuts_the_wait_short_and_one_given_before_ends_the_next_at_once() {
    let interrupt = soon();
    let nudge = interrupt.nudge();
    let later = nudge.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        later.nudge();
    });
    let started = Instant::now();
    let ended = with(
        Script::javascript(
            "const a = await wait(20); const b = await wait(0.05); return [a, b].join(' ')",
        ),
        Kept::default(),
        Limits::default(),
        interrupt,
    )
    .await;
    assert!(ended.ok(), "{}", ended.text);
    assert_eq!(
        ended.text, "true false",
        "the first cut short, the second slept"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    // Given while no wait runs, it counts for the next one — once.
    let interrupt = soon();
    interrupt.nudge().nudge();
    interrupt.nudge().nudge();
    let started = Instant::now();
    let ended = with(
        Script::javascript(
            "const a = await wait(20); const b = await wait(0.05); return [a, b].join(' ')",
        ),
        Kept::default(),
        Limits::default(),
        interrupt,
    )
    .await;
    assert_eq!(ended.text, "true false", "{}", ended.text);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn a_script_that_waits_for_nothing_ends_at_once() {
    let started = Instant::now();
    let ended = js("text('a'); await new Promise(() => {}); return 'never'").await;
    assert_eq!(ended.ending, Ending::Stuck, "{}", ended.text);
    assert!(
        ended
            .text
            .starts_with("a\nScript ended: it waits for something"),
        "{}",
        ended.text
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn wait_pauses_within_its_limit_and_is_no_stuck_script() {
    let limits = Limits {
        max_wait: Duration::from_secs(1),
        ..Limits::default()
    };
    let started = Instant::now();
    let ended = with(
        Script::javascript("await wait(0.3); text('after'); await wait(0); return 'done'"),
        Kept::default(),
        limits,
        soon(),
    )
    .await;
    assert!(ended.ok(), "{}", ended.text);
    assert_eq!(ended.text, "after\ndone");
    assert!(started.elapsed() >= Duration::from_millis(300));
    let refused = with(
        Script::javascript("try { await wait(5) } catch (e) { return e.message }"),
        Kept::default(),
        limits,
        soon(),
    )
    .await;
    assert_eq!(refused.text, "wait at most 1 seconds at a time");
    assert_eq!(
        js("try { await wait('x') } catch (e) { return e.message }")
            .await
            .text,
        "wait needs a number of seconds"
    );
}

#[tokio::test]
async fn calls_past_the_limit_are_refused_and_the_script_goes_on() {
    let limits = Limits {
        max_calls: 2,
        ..Limits::default()
    };
    let ended = with(
        Script::javascript(
            "const r = await Promise.allSettled([1, 2, 3].map(n => tools.echo({text: String(n)})));\n\
             return r.map(x => x.status === 'fulfilled' ? x.value : x.reason.message).join('|')",
        ),
        Kept::default(),
        limits,
        soon(),
    )
    .await;
    assert!(ended.ok(), "{}", ended.text);
    assert_eq!(
        ended.text,
        "echo:1|echo:2|too many tool calls: at most 2 per script"
    );
    assert_eq!(ended.calls, 2);
}

#[tokio::test]
async fn typescript_runs_without_its_types_and_broken_typescript_says_why() {
    let ended = with(
        Script::typescript(
            "interface Hit { text: string }\n\
             const said: string = await tools.echo({ text: 'ts' });\n\
             const hits: Hit[] = [{ text: said }];\n\
             return hits.map((h: Hit): string => h.text).join();",
        ),
        Kept::default(),
        Limits::default(),
        soon(),
    )
    .await;
    assert!(ended.ok(), "{}", ended.text);
    assert_eq!(ended.text, "echo:ts");
    let broken = with(
        Script::typescript("const x: = 1;"),
        Kept::default(),
        Limits::default(),
        soon(),
    )
    .await;
    assert_eq!(broken.ending, Ending::Failed);
    assert!(
        broken
            .text
            .starts_with("error: the TypeScript could not be read: "),
        "{}",
        broken.text
    );
}

#[tokio::test]
async fn long_output_keeps_head_and_tail_and_says_it_was_cut() {
    let limits = Limits {
        max_output_chars: 30,
        ..Limits::default()
    };
    let ended = with(
        Script::javascript("return 'h'.repeat(50) + 't'.repeat(50)"),
        Kept::default(),
        limits,
        soon(),
    )
    .await;
    assert!(ended.cut);
    assert_eq!(
        ended.text,
        format!(
            "{}\n[… 70 characters left out …]\n{}",
            "h".repeat(20),
            "t".repeat(10)
        )
    );
}

#[tokio::test]
async fn memory_past_the_limit_fails_the_script_not_the_host() {
    let limits = Limits {
        memory_bytes: 8 * 1024 * 1024,
        ..Limits::default()
    };
    let ended = with(
        Script::javascript("const a = []; while (true) a.push('x'.repeat(1024 * 1024));"),
        Kept::default(),
        limits,
        soon(),
    )
    .await;
    assert_eq!(ended.ending, Ending::Failed, "{}", ended.text);
}

#[test]
fn the_arguments_name_the_code_and_the_language() {
    assert_eq!(
        Script::from_arguments(&json!({"code": "```js\nreturn 1\n```"})),
        Ok(Script::javascript("return 1"))
    );
    assert_eq!(
        Script::from_arguments(&json!({"code": "return 1", "language": "TS"})),
        Ok(Script::typescript("return 1"))
    );
    assert_eq!(
        Script::from_arguments(&json!({"code": "  "})),
        Err(ArgumentsError::NoCode)
    );
    assert_eq!(
        Script::from_arguments(&json!([1])),
        Err(ArgumentsError::NotAnObject)
    );
    assert_eq!(
        Script::from_arguments(&json!({"code": "x", "language": "python"})),
        Err(ArgumentsError::Language("python".into()))
    );
}

#[test]
fn the_guide_names_the_limits() {
    let guide = guide(&Limits::default());
    assert!(
        guide.contains("At most 30 tool calls") && guide.contains("at most 30 s at a time"),
        "{guide}"
    );
}

#[tokio::test]
async fn a_huge_wait_is_refused_not_a_panic() {
    let ended = js("try { await wait(1e30) } catch (e) { return e.message }").await;
    assert_eq!(ended.text, "wait at most 30 seconds at a time");
}

#[tokio::test]
async fn two_scripts_at_once_keep_both_their_values() {
    let kept = Kept::default();
    let first = with(
        Script::javascript("store('a', 1); await wait(0.2); return 'a'"),
        Arc::clone(&kept),
        Limits::default(),
        soon(),
    );
    let second = with(
        Script::javascript("store('b', 2); return 'b'"),
        Arc::clone(&kept),
        Limits::default(),
        soon(),
    );
    let (a, b) = tokio::join!(first, second);
    assert!(a.ok() && b.ok());
    let both = js_with_kept("return [load('a'), load('b')].join(',')", &kept).await;
    assert_eq!(both.text, "1,2");
}

async fn js_with_kept(code: &str, kept: &Kept) -> Ended {
    with(
        Script::javascript(code),
        Arc::clone(kept),
        Limits::default(),
        soon(),
    )
    .await
}

#[test]
fn deep_recursion_is_an_error_in_the_script_on_a_small_thread() {
    // A thread of 1 MiB, the least the crate asks for: the recursion ends inside QuickJS.
    let ended = std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap()
                .block_on(js(
                    "function f() { return f() + 1 }; try { f() } catch (e) { return e.name }",
                ))
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(ended.text, "RangeError");
}

#[tokio::test]
async fn many_empty_lines_are_bounded_and_quick() {
    let started = Instant::now();
    let ended = js("for (let i = 0; i < 200000; i++) console.log(''); return 'end'").await;
    assert!(ended.ok(), "{}", ended.text);
    assert!(
        ended.text.contains("[… more output dropped"),
        "{}",
        ended.text
    );
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn errors_maps_sets_and_odd_values_print_readably() {
    assert_eq!(
        js("try { throw new TypeError('bad') } catch (e) { console.log(e) }")
            .await
            .text,
        "TypeError: bad"
    );
    assert_eq!(
        js("return [new Map([['a', 1]]), new Set([2])]").await.text,
        "[\n  {\n    \"a\": 1\n  },\n  [\n    2\n  ]\n]"
    );
    let big = js("store('k', 1); return 1n").await;
    assert!(big.ok(), "{}", big.text);
    assert_eq!(big.text, "1");
    let circular = js("const o = {}; o.o = o; return o").await;
    assert!(
        circular.ok() && circular.text == "[object Object]",
        "{}",
        circular.text
    );
}

#[tokio::test]
async fn whatever_is_thrown_reads_as_an_error_with_the_scripts_own_line() {
    let plain = js("throw 'plain'").await;
    assert!(
        plain.text.starts_with("Script error: Error: plain"),
        "{}",
        plain.text
    );
    let object = js("throw {a: 1}").await;
    assert!(
        object.text.contains("\"a\": 1") && !object.text.contains("0x"),
        "{}",
        object.text
    );
    let line = js("const a = 1;\nnull.x;").await;
    assert!(line.text.contains(":2"), "{}", line.text);
}

#[tokio::test]
async fn store_says_plainly_why_it_refused() {
    let ended = js(&format!(
        "try {{ store('k', 'x'.repeat({})) }} catch (e) {{ return e.message }}",
        MAX_STORED_VALUE_CHARS + 1
    ))
    .await;
    assert_eq!(ended.text, "store: the value is too large");
}

#[tokio::test]
async fn too_long_a_script_is_refused_before_it_runs() {
    let limits = Limits {
        max_source_chars: 10,
        ..Limits::default()
    };
    let ended = with(
        Script::javascript("return 'far too long'"),
        Kept::default(),
        limits,
        soon(),
    )
    .await;
    assert_eq!(ended.text, "error: the script is longer than 10 characters");
}

#[test]
fn an_empty_fence_is_no_code_and_the_language_keeps_its_spelling() {
    assert_eq!(
        Script::from_arguments(&json!({"code": "``` ```"})),
        Err(ArgumentsError::NoCode)
    );
    assert_eq!(
        Script::from_arguments(&json!({"code": "x", "language": "Python"})),
        Err(ArgumentsError::Language("Python".into()))
    );
}

#[tokio::test]
async fn huge_array_likes_are_refused_before_a_native_loop_holds_the_thread() {
    let started = Instant::now();
    for code in [
        "const a = new Array(4294967295); try { a.reverse() } catch (e) { return e.name }",
        "try { Array.prototype.join.call({length: 2 ** 53 - 1}, '') } catch (e) { return e.name }",
        "const a = new Array(4294967295); try { a.forEach(() => {}) } catch (e) { return e.name }",
        "try { Array.from({length: 2 ** 32}) } catch (e) { return e.name }",
    ] {
        assert_eq!(js(code).await.text, "RangeError", "{code}");
    }
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(
        js("return [3, 1, 2].sort().reverse().join('')").await.text,
        "321"
    );
}

#[tokio::test]
async fn the_hosts_helpers_are_out_of_reach_and_calls_are_checked() {
    assert_eq!(
        js("return [typeof __call, typeof __emit, typeof __store, typeof __max_calls].join(',')")
            .await
            .text,
        "undefined,undefined,undefined,undefined"
    );
    assert_eq!(
        js("try { await tools.echo([1, 2]) } catch (e) { return e.message }")
            .await
            .text,
        "the arguments must be an object"
    );
    assert_eq!(
        js("try { await tools['a\\nb'] ({}) } catch (e) { return e.message }")
            .await
            .text,
        "there is no tool ab for scripts"
    );
}

#[tokio::test]
async fn match_search_and_match_all_fail_plainly_without_regular_expressions() {
    for code in [
        "'abc'.match('b')",
        "'abc'.search('b')",
        "[...'abc'.matchAll('b')]",
        "eval('/a/')",
    ] {
        let ended = js(&format!(
            "try {{ {code}; return 'ran' }} catch (e) {{ return e.name }}"
        ))
        .await;
        assert_ne!(ended.text, "ran", "{code}");
    }
    assert_eq!(js("return typeof RegExp").await.text, "undefined");
}

#[tokio::test]
async fn a_request_knows_when_its_script_was_stopped() {
    let (tx, mut rx) = mpsc::channel::<Request>(8);
    let interrupt = soon();
    let stop = interrupt.stopper();
    let running = tokio::spawn(run(
        Script::javascript("await tools.echo({text: 'x'})"),
        echo_catalog(),
        Kept::default(),
        Limits::default(),
        interrupt,
        tx,
    ));
    let request = rx.recv().await.unwrap();
    assert!(!request.is_cancelled());
    stop.store(true, Ordering::SeqCst);
    assert!(request.is_cancelled());
    assert_eq!(running.await.unwrap().ending, Ending::Interrupted);
}
