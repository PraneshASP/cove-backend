use serde_json::json;
mod common;

#[tokio::test]
async fn verify_test() {
    let app = common::spawn_app().await;
    let client = reqwest::Client::new();

    // let body = json!({
    //     "repo_url": "https://github.com/ScopeLift/cove-test-repo",
    //     "repo_commit": "188587df6652484e64590127f6ae3038c0aa93e3",
    //     "contract_address": "0x406B940c7154eDB4Aa2B20CA62fC9A7e70fbe435",
    // });
    let body = json!({
        "repo_url": "https://github.com/ProjectOpenSea/seaport",
        "repo_commit": "d58a91d218b0ab557543c8a292710aa36e693973",
        "contract_address": "0x00000000000001ad428e4906aE43D8F9852d0dD6",
    });
    let response = client
        .post(&format!("{}/verify", app.address))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .expect("Failed to execute request.");

    println!("status   {:?}", response.status());
    // println!("response {:?}", response.text().await);

    assert_eq!(200, response.status().as_u16());

    // let saved = ...
    // assert_eq!();
}

#[tokio::test]
async fn verify_returns_a_400_when_repo_cannot_be_cloned() {
    let app = common::spawn_app().await;
    let client = reqwest::Client::new();

    let body = json!({
        "repo_url": "https://github.com/ScopeLift/non-existant-repo",
        "repo_commit": "14a113dd794d4938da7e0e12828434d666eb9a31",
        "contract_address": "0x1908e2bf4a88f91e4ef0dc72f02b8ea36bea2319",
    });
    let response = client
        .post(&format!("{}/verify", app.address))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .expect("Failed to execute request.");

    assert_eq!(400, response.status().as_u16(),);
}

#[tokio::test]
async fn verify_returns_a_400_when_repo_has_no_foundry_toml() {
    let app = common::spawn_app().await;
    let client = reqwest::Client::new();

    let body = json!({
        "repo_url": "https://github.com/ScopeLift/scopelint",
        "repo_commit": "14a113dd794d4938da7e0e12828434d666eb9a31",
        "contract_address": "0x1908e2bf4a88f91e4ef0dc72f02b8ea36bea2319",
    });
    let response = client
        .post(&format!("{}/verify", app.address))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .expect("Failed to execute request.");

    assert_eq!(400, response.status().as_u16(),);
}
#[tokio::test]
async fn verify_returns_a_400_when_data_is_missing() {
    let app = common::spawn_app().await;
    let client = reqwest::Client::new();

    let body1 = json!({
        "repo_commit": "abcdef1",
        "contract_address": "0x123",
    });

    let body2 = json!({
        "repo_url": "https://github.com/ScopeLift/cove-test-repo",
        "repo_commit": "abcdef1",
    });

    // TODO Test more combinations.
    let test_cases = vec![(body1, "missing repo_url"), (body2, "missing contract_address")];

    for (invalid_body, error_message) in test_cases {
        let response = client
            .post(&format!("{}/verify", app.address))
            .header("Content-Type", "application/json")
            .body(invalid_body.to_string())
            .send()
            .await
            .expect("Failed to execute request.");

        assert_eq!(
            422,
            response.status().as_u16(),
            "Wrong response for payload:
{error_message}"
        );
    }
}
