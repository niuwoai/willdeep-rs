use super::{DaemonState, TOKEN_HEADER};
use axum::http::{HeaderMap, StatusCode};
use serde::{Serialize, de::DeserializeOwned};

pub(crate) const HEADER: &str = "x-willdeep-internal-transport";
const HEADER_VALUE: &str = "1";

pub(crate) struct InternalRuntimeClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum InternalTransportError {
    #[error("internal Runtime HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("internal Runtime request failed: HTTP {status}: {message}")]
    HttpStatus { status: u16, message: String },
}

impl InternalTransportError {
    pub(crate) fn status_code(&self) -> Option<u16> {
        match self {
            Self::Http(error) => error.status().map(|status| status.as_u16()),
            Self::HttpStatus { status, .. } => Some(*status),
        }
    }
}

impl InternalRuntimeClient {
    pub(crate) fn from_state(state: &DaemonState) -> Result<Self, InternalTransportError> {
        Self::new(format!("http://{}", state.address), state.token.clone())
    }

    pub(crate) fn new(base_url: String, token: String) -> Result<Self, InternalTransportError> {
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            token,
            http: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(2))
                .build()?,
        })
    }

    pub(crate) async fn get<T>(&self, path: &str) -> Result<T, InternalTransportError>
    where
        T: DeserializeOwned,
    {
        let response = self.request(self.http.get(self.url(path))).send().await?;
        decode(response).await
    }

    pub(crate) async fn post<P, T>(
        &self,
        path: &str,
        payload: &P,
    ) -> Result<T, InternalTransportError>
    where
        P: Serialize + ?Sized,
        T: DeserializeOwned,
    {
        let response = self
            .request(self.http.post(self.url(path)))
            .json(payload)
            .send()
            .await?;
        decode(response).await
    }

    pub(crate) async fn post_empty(&self, path: &str) -> Result<(), InternalTransportError> {
        let response = self.request(self.http.post(self.url(path))).send().await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(InternalTransportError::HttpStatus {
                status: response.status().as_u16(),
                message: response.text().await.unwrap_or_default(),
            })
        }
    }

    fn request(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request
            .header(TOKEN_HEADER, &self.token)
            .header(HEADER, HEADER_VALUE)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }
}

pub(crate) fn authorize(headers: &HeaderMap) -> Result<(), StatusCode> {
    let valid = headers
        .get(HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == HEADER_VALUE);
    if valid {
        Ok(())
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

async fn decode<T: DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, InternalTransportError> {
    let status = response.status();
    if !status.is_success() {
        let message = response.text().await.unwrap_or_default();
        return Err(InternalTransportError::HttpStatus {
            status: status.as_u16(),
            message,
        });
    }
    response.json().await.map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_marker_is_required_exactly() {
        let mut headers = HeaderMap::new();
        assert_eq!(authorize(&headers), Err(StatusCode::NOT_FOUND));
        headers.insert(HEADER, "0".parse().unwrap());
        assert_eq!(authorize(&headers), Err(StatusCode::NOT_FOUND));
        headers.insert(HEADER, HEADER_VALUE.parse().unwrap());
        assert_eq!(authorize(&headers), Ok(()));
    }
}
