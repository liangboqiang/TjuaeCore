//! Integration coverage for canonical assistant-rule dispatch.

use std::sync::{Arc, Mutex};

use axum::Extension;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tjuaeui_api_types::ApiResponse;
use tjuaeui_asset::{
    AssetCatalogService, AssetError, AssistantRuleDispatcher, SkillPaths, SkillRouterState, skill_routes,
};
use tjuaeui_auth::CurrentUser;
use tower::ServiceExt;

#[derive(Default)]
struct CallLog {
    reads: Vec<(String, String, Option<String>)>,
}

struct FakeDispatcher {
    content: String,
    log: Mutex<CallLog>,
}

#[async_trait::async_trait]
impl AssistantRuleDispatcher for FakeDispatcher {
    async fn read_rule(&self, user_id: &str, id: &str, locale: Option<&str>) -> Result<String, AssetError> {
        self.log
            .lock()
            .unwrap()
            .reads
            .push((user_id.to_owned(), id.to_owned(), locale.map(str::to_owned)));
        Ok(self.content.clone())
    }
}

async fn state(dispatcher: Option<Arc<FakeDispatcher>>) -> SkillRouterState {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path().to_path_buf();
    let database = tjuaeui_db::init_database_memory().await.unwrap();
    let asset_repo: Arc<dyn tjuaeui_db::IAssetRepository> =
        Arc::new(tjuaeui_db::SqliteAssetRepository::new(database.pool().clone()));
    let asset_catalog = Arc::new(AssetCatalogService::new(asset_repo, &root));
    std::mem::forget(temp);

    SkillRouterState {
        skill_paths: SkillPaths {
            user_skills_dir: root.join("skills"),
            cron_skills_dir: root.join("cron").join("skills"),
        },
        asset_catalog,
        assistant_dispatcher: dispatcher.map(|dispatcher| dispatcher as Arc<dyn AssistantRuleDispatcher>),
    }
}

async fn router(dispatcher: Arc<FakeDispatcher>) -> axum::Router {
    skill_routes(state(Some(dispatcher)).await).layer(Extension(CurrentUser {
        id: "user-1".into(),
        username: "tester".into(),
    }))
}

fn json_request(method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

async fn body_json<T: serde::de::DeserializeOwned>(response: axum::response::Response) -> T {
    let body = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn assistant_rule_read_is_user_scoped() {
    let dispatcher = Arc::new(FakeDispatcher {
        content: "rule body".into(),
        log: Mutex::new(CallLog::default()),
    });
    let response = router(dispatcher.clone())
        .await
        .oneshot(json_request(
            "POST",
            "/api/skills/assistant-rule/read",
            serde_json::json!({"assistant_id": "assistant-1", "locale": "zh-CN"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: ApiResponse<String> = body_json(response).await;
    assert_eq!(body.data.as_deref(), Some("rule body"));
    assert_eq!(
        dispatcher.log.lock().unwrap().reads,
        vec![("user-1".into(), "assistant-1".into(), Some("zh-CN".into()))]
    );
}

#[tokio::test]
async fn skill_routes_fail_closed_without_current_user() {
    let dispatcher = Arc::new(FakeDispatcher {
        content: "rule body".into(),
        log: Mutex::new(CallLog::default()),
    });
    let response = skill_routes(state(Some(dispatcher.clone())).await)
        .oneshot(json_request(
            "POST",
            "/api/skills/assistant-rule/read",
            serde_json::json!({"assistant_id": "assistant-1"}),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(dispatcher.log.lock().unwrap().reads.is_empty());
}

#[tokio::test]
async fn empty_materialize_is_supported_for_any_authenticated_user() {
    let dispatcher = Arc::new(FakeDispatcher {
        content: String::new(),
        log: Mutex::new(CallLog::default()),
    });
    let response = router(dispatcher)
        .await
        .oneshot(json_request(
            "POST",
            "/api/skills/materialize-for-agent",
            serde_json::json!({"conversation_id": "conversation-1", "skills": []}),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn assistant_rule_routes_fail_closed_without_canonical_dispatcher() {
    let response = skill_routes(state(None).await)
        .layer(Extension(CurrentUser {
            id: "user-1".into(),
            username: "tester".into(),
        }))
        .oneshot(json_request(
            "POST",
            "/api/skills/assistant-rule/read",
            serde_json::json!({"assistant_id": "assistant-1"}),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}
