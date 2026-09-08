use lime_protocol::{InputRequest, InputResponse, Request, Response};

#[test]
fn contract_examples_match_rust_wire_types() {
    let input: InputRequest =
        serde_json::from_str(include_str!("../../../contracts/ipc.request.example.json"))
            .expect("input request example must match protocol");
    assert_eq!(input.preedit, "nihao");

    let response: InputResponse =
        serde_json::from_str(include_str!("../../../contracts/ipc.response.example.json"))
            .expect("input response example must match protocol");
    assert_eq!(response.candidates.len(), 1);

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
}
