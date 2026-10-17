//! A passing `dry_run=true` is remembered on the plan and satisfies the
//! `require_dry_run` guard on the following `dry_run=false` apply. The dry run
//! itself goes to `googleAds:mutate` with `validateOnly: true`; a failing dry
//! run leaves the plan unmarked so the guard still blocks.
//!
//! Both scenarios share one test function (and one mock server) because the
//! client reads `GOOGLE_ADS_API_BASE_URL` from the process environment.

mod common;

use mcp_google_ads::error::McpGoogleAdsError;
use mcp_google_ads::safety::preview::{get_plan, remove_plan, store_plan, ChangePlan};
use mcp_google_ads::tools::confirm::{confirm_and_apply, ConfirmApplyInput};
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, ResponseTemplate};

fn plan_for(customer_id: &str) -> ChangePlan {
    ChangePlan::new(
        "draft_campaign".into(),
        "campaign".into(),
        "new".into(),
        customer_id.into(),
        json!({"name": "Wizardi Autumn"}),
        false,
        vec![json!({"campaignOperation": {"create": {"name": "Wizardi Autumn"}}})],
    )
}

#[tokio::test]
async fn dry_run_then_apply_passes_guard_and_failed_dry_run_does_not() {
    let mock = wiremock::MockServer::start().await;

    // ── Happy path: customer 1111111111 ──────────────────────────────────
    // The dry run must carry validateOnly=true and must NOT be counted as a
    // real write; the apply must carry validateOnly=false.
    Mock::given(method("POST"))
        .and(path("/v25/customers/1111111111/googleAds:mutate"))
        .and(body_partial_json(json!({"validateOnly": true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/v25/customers/1111111111/googleAds:mutate"))
        .and(body_partial_json(json!({"validateOnly": false})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "mutateOperationResponses": [
                {"campaignOperation": {"resourceName": "customers/1111111111/campaigns/42"}}
            ]
        })))
        .expect(1)
        .mount(&mock)
        .await;

    // ── Failing validation: customer 2222222222 ──────────────────────────
    Mock::given(method("POST"))
        .and(path("/v25/customers/2222222222/googleAds:mutate"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {
                "code": 400,
                "status": "INVALID_ARGUMENT",
                "message": "Request contains an invalid argument.",
                "details": [{
                    "@type": "type.googleapis.com/google.ads.googleads.v25.errors.GoogleAdsFailure",
                    "errors": [{
                        "errorCode": {"campaignError": "MISSING_BIDDING_STRATEGY"},
                        "message": "The campaign needs a bidding strategy."
                    }]
                }]
            }
        })))
        .expect(1)
        .mount(&mock)
        .await;

    std::env::set_var("GOOGLE_ADS_API_BASE_URL", format!("{}/v25", mock.uri()));

    let mut config = common::test_config();
    config.safety.require_dry_run = true; // the default in production

    // ── Happy path ───────────────────────────────────────────────────────
    let plan = plan_for("1111111111");
    let good_id = plan.plan_id.clone();
    assert!(!plan.dry_run_completed);
    store_plan(plan);

    let preview = confirm_and_apply(
        &config,
        ConfirmApplyInput {
            plan_id: good_id.clone(),
            dry_run: true,
            ..Default::default()
        },
    )
    .await
    .expect("dry run should pass");
    assert_eq!(preview["dry_run"], true);
    assert_eq!(preview["dry_run_completed"], true);
    assert_eq!(preview["validation"]["status"], "PASSED");
    assert_eq!(preview["validation"]["method"], "validateOnly");
    assert!(
        get_plan(&good_id)
            .expect("plan kept after dry run")
            .dry_run_completed,
        "dry run must be remembered on the stored plan"
    );

    let applied = confirm_and_apply(
        &config,
        ConfirmApplyInput {
            plan_id: good_id.clone(),
            dry_run: false,
            bypass_require_dry_run: false, // no bypass needed any more
            ..Default::default()
        },
    )
    .await
    .expect("apply must pass the guard after a dry run");
    assert_eq!(applied["status"], "APPLIED");
    assert!(get_plan(&good_id).is_none(), "plan removed after apply");

    // ── Failing validation ───────────────────────────────────────────────
    let plan = plan_for("2222222222");
    let bad_id = plan.plan_id.clone();
    store_plan(plan);

    let err = confirm_and_apply(
        &config,
        ConfirmApplyInput {
            plan_id: bad_id.clone(),
            dry_run: true,
            ..Default::default()
        },
    )
    .await
    .expect_err("dry run must surface the API validation error");
    assert!(
        !matches!(err, McpGoogleAdsError::DryRunRequired),
        "a failing dry run is an API error, not the guard"
    );
    let stored = get_plan(&bad_id).expect("plan kept after failed dry run so it can be fixed");
    assert!(
        !stored.dry_run_completed,
        "a failing dry run must not unlock the guard"
    );

    let err = confirm_and_apply(
        &config,
        ConfirmApplyInput {
            plan_id: bad_id.clone(),
            dry_run: false,
            ..Default::default()
        },
    )
    .await
    .expect_err("guard must still block");
    assert!(matches!(err, McpGoogleAdsError::DryRunRequired));

    // Cleanup
    std::env::remove_var("GOOGLE_ADS_API_BASE_URL");
    remove_plan(&bad_id);
}
