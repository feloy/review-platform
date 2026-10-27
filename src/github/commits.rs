//! Read PR commit co-author trailers as untrusted model-selection hints.

use super::{GithubClient, GithubError, GithubResult, HttpResponse, PullRequest};
use crate::config::CoauthorIdentity;
use crate::{HeadSha, PrKey};
use serde::Deserialize;
use std::collections::HashSet;
use std::io::Read;

const PAGE_SIZE: usize = 100;
// GitHub's list PR commits endpoint returns at most 250 commits.
const MAX_COMMITS: usize = 250;
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Co-author hints obtained from a stable read of the PR commit list.
pub struct CommitCoauthors {
    head_sha: HeadSha,
    identities: HashSet<CoauthorIdentity>,
    commit_count: usize,
}

impl CommitCoauthors {
    pub fn head_sha(&self) -> &HeadSha {
        &self.head_sha
    }

    pub fn identities(&self) -> &HashSet<CoauthorIdentity> {
        &self.identities
    }

    pub fn commit_count(&self) -> usize {
        self.commit_count
    }
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
struct ApiCommit {
    sha: HeadSha,
    commit: ApiCommitDetails,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
struct ApiCommitDetails {
    message: String,
}

impl GithubClient {
    pub fn coauthors_for_pull(
        &self,
        key: &PrKey,
        pull: &PullRequest,
    ) -> GithubResult<CommitCoauthors> {
        let first = self.collect_pull_commits(key)?;
        let second = self.collect_pull_commits(key)?;
        if first != second {
            return Err(GithubError::PaginationChanged {
                kind: "PR commits",
                detail: "commits changed between retrieval passes".to_owned(),
            });
        }
        self.ensure_unchanged(key, pull, false)?;
        if second.last().map(|commit| &commit.sha) != Some(pull.head_sha()) {
            if second.len() == MAX_COMMITS {
                return Err(GithubError::PrCommitsCapped {
                    key: key.clone(),
                    limit: MAX_COMMITS,
                });
            }
            return Err(GithubError::InvalidResponse {
                resource: format!("PR commits for {key}"),
                detail: "last commit does not match the inspected PR head".to_owned(),
            });
        }
        let identities = second
            .iter()
            .flat_map(|commit| trailer_coauthors(&commit.commit.message))
            .collect();
        Ok(CommitCoauthors {
            head_sha: pull.head_sha().clone(),
            identities,
            commit_count: second.len(),
        })
    }

    fn collect_pull_commits(&self, key: &PrKey) -> GithubResult<Vec<ApiCommit>> {
        let mut commits = Vec::new();
        let mut seen = HashSet::new();
        let mut bytes_read = 0;
        let resource = format!("PR commits for {key}");
        for page in 1..=(MAX_COMMITS / PAGE_SIZE + 1) {
            let route = format!(
                "/repos/{}/pulls/{}/commits?per_page={PAGE_SIZE}&page={page}",
                key.repository(),
                key.number()
            );
            let response = self.get(&route, "application/vnd.github+json", &resource)?;
            let has_next = match response.headers().get("Link") {
                None => false,
                Some(header) => header
                    .to_str()
                    .map_err(|_| GithubError::InvalidResponse {
                        resource: resource.clone(),
                        detail: "invalid Link header".to_owned(),
                    })?
                    .contains("rel=\"next\""),
            };
            let items: Vec<ApiCommit> = read_page(response, &resource, &mut bytes_read)?;
            let items_len = items.len();
            if items.len() > PAGE_SIZE {
                return Err(GithubError::InvalidResponse {
                    resource: resource.clone(),
                    detail: format!("page {page} contains more than {PAGE_SIZE} commits"),
                });
            }
            for item in items {
                if !seen.insert(item.sha.clone()) {
                    return Err(GithubError::PaginationChanged {
                        kind: "PR commits",
                        detail: format!("commit {} appeared twice", item.sha),
                    });
                }
                commits.push(item);
            }
            if commits.len() > MAX_COMMITS {
                return Err(GithubError::TooManyResults {
                    kind: "PR commits",
                    limit: MAX_COMMITS,
                });
            }
            if has_next && commits.len() == MAX_COMMITS {
                return Err(GithubError::TooManyResults {
                    kind: "PR commits",
                    limit: MAX_COMMITS,
                });
            }
            if has_next && items_len != PAGE_SIZE {
                return Err(GithubError::PaginationChanged {
                    kind: "PR commits",
                    detail: "short page has a next link".to_owned(),
                });
            }
            if !has_next && items_len < PAGE_SIZE {
                return Ok(commits);
            }
        }
        Err(GithubError::TooManyResults {
            kind: "PR commits",
            limit: MAX_COMMITS,
        })
    }
}

fn read_page(
    response: HttpResponse,
    resource: &str,
    bytes_read: &mut u64,
) -> GithubResult<Vec<ApiCommit>> {
    let remaining = MAX_BYTES - *bytes_read;
    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(remaining + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| GithubError::ReadResponse {
            resource: resource.to_owned(),
            source,
        })?;
    *bytes_read += bytes.len() as u64;
    if *bytes_read > MAX_BYTES {
        return Err(GithubError::ResponseTooLarge {
            kind: "PR commits",
            limit_mib: MAX_BYTES / (1024 * 1024),
        });
    }
    serde_json::from_slice(&bytes).map_err(|source| GithubError::DecodeResponse {
        resource: resource.to_owned(),
        source,
    })
}

fn trailer_coauthors(message: &str) -> Vec<CoauthorIdentity> {
    let mut identities = Vec::new();
    for line in message
        .lines()
        .rev()
        .skip_while(|line| line.trim().is_empty())
    {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            break;
        };
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            break;
        }
        if !key.eq_ignore_ascii_case("Co-authored-by") {
            continue;
        }
        let value = value.trim();
        if let Some(identity) = value.rsplit_once('<').and_then(|(name, email)| {
            email
                .strip_suffix('>')
                .and_then(|email| CoauthorIdentity::new(name, email).ok())
        }) {
            identities.push(identity);
        }
    }
    identities
}

#[cfg(test)]
mod tests {
    use super::trailer_coauthors;

    #[test]
    fn reads_only_trailing_coauthor_model_signatures() {
        let message = "Co-authored-by: Fake <fake@example.com>\n\nChange implementation\n\nSigned-off-by: Dev <dev@example.com>\nCo-Authored-By: Claude <Claude@Example.com>\n";
        let coauthors = trailer_coauthors(message);
        assert_eq!(coauthors.len(), 1);
        assert_eq!(coauthors[0].name(), "claude");
        assert_eq!(coauthors[0].email(), "claude@example.com");
        assert!(trailer_coauthors("Change\n\nCo-authored-by: no email").is_empty());
    }
}
