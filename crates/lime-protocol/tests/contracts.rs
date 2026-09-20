use lime_protocol::{InputRequest, InputResponse, Request, Response};

#[test]
fn contract_examples_match_rust_wire_types() {
    let input: InputRequest =
        serde_json::from_str(include_str!("../../../contracts/ipc.request.example.json"))
            .expect("input request example must match protocol");
    assert_eq!(input.preedit, "nihao");
    assert_eq!(input.candidate_extension_of, None);

    let extension: InputRequest = serde_json::from_str(
        r#"{"request_id":2,"preedit":"nihao","preceding_text":"你好，","context_available":true,"config_revision":1,"candidate_extension_of":1,"candidate_limit":64}"#,
    )
    .expect("candidate extension request must match protocol");
    assert_eq!(extension.candidate_extension_of, Some(1));

    let response: InputResponse =
        serde_json::from_str(include_str!("../../../contracts/ipc.response.example.json"))
            .expect("input response example must match protocol");
    assert_eq!(response.candidates.len(), 1);
    assert_eq!(response.candidate_remainders, vec![Some(String::new())]);

    let management_request: Request = serde_json::from_str(include_str!(
        "../../../contracts/ipc.management.request.example.json"
    ))
    .expect("management request example must match protocol");
    assert!(matches!(
        management_request,
        Request::GetDictionaryPage { page: 1, .. }
    ));

    let management_response: Response = serde_json::from_str(include_str!(
        "../../../contracts/ipc.management.response.example.json"
    ))
    .expect("management response example must match protocol");
    assert!(matches!(management_response, Response::Status(_)));

    let benchmark_request: Request = serde_json::from_str(include_str!(
        "../../../contracts/ipc.benchmark.request.example.json"
    ))
    .expect("benchmark request example must match protocol");
    let Request::StartBenchmark(request) = benchmark_request else {
        panic!("expected benchmark batch request");
    };
    assert_eq!(request.models.len(), 2);
    assert_eq!(request.configurations.len(), 2);
    assert_eq!(request.modes.len(), 2);
    let benchmark_response: Response = serde_json::from_str(include_str!(
        "../../../contracts/ipc.benchmark.response.example.json"
    ))
    .expect("benchmark response example must match protocol");
    let Response::BenchmarkState(state) = benchmark_response else {
        panic!("expected benchmark batch response");
    };
    assert_eq!(state.results.len(), 8);
    assert_eq!(state.results[0].configuration.llm_rerank_count, 16);
}
