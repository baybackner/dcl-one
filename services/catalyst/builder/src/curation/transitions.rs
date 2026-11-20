//! Who may move a curation from one status to another: the one table every
//! curation write decides by. A row is the kind of curation, the status of
//! its newest row (none when it has none), the request and who sends it; the
//! answer is the change to write or the refusal.
//!
//! | Curation | From | Request | Committee | Creator |
//! | --- | --- | --- | --- | --- |
//! | collection | any | `PATCH` assignee | the newest row, reassigned (404 without one) | 401 |
//! | collection | any | `PATCH` status | the newest row, to that status (404 without one) | 401 |
//! | collection | none, approved, rejected | `POST` | a new pending row, assigned when asked | a new pending row (401 with an assignee) |
//! | collection | pending | `POST` | 400 | 400 |
//! | item | any | `PATCH` assignee | 400 | 401 |
//! | item | any | `PATCH` approved or rejected | the newest row, to that status (404 without one) | 401 |
//! | item | pending | `PATCH` pending | the newest row, its content hash taken again | the same |
//! | item | approved, rejected | `PATCH` pending | the newest row, back to pending | 401 |
//! | item | none | `PATCH` | 404 | 404 (401 for approved or rejected) |
//! | item | approved, rejected | `POST` | a new pending row | the same |
//! | item | pending | `POST` | 400 | 400 |
//! | item | none | `POST` | 400 | 400 |
//!
//! A creator is any wallet with a role on the collection (its owner, an
//! on-chain manager, its provider's manager; for an item's curation, the
//! item's owner) that the committee did not admit. Every write the committee
//! makes is recorded in `ops.admin_audit`.

/// A curation's status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pending,
    Approved,
    Rejected,
}

impl Status {
    pub const ALL: [Status; 3] = [Status::Pending, Status::Approved, Status::Rejected];

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Approved => "approved",
            Status::Rejected => "rejected",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == raw)
    }
}

/// Whose curation it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curation {
    Collection,
    Item,
}

/// Who sends the request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Actor {
    Committee,
    Creator,
}

/// What the request asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// `PATCH`: a status, a new assignee, or both.
    Update {
        status: Option<Status>,
        assignee: bool,
    },
    /// `POST`: a new review request, assigned when `assignee`.
    Open { assignee: bool },
}

/// The write a request is allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Update the newest row.
    Update {
        status: Option<Status>,
        assignee: bool,
    },
    /// Insert a new pending row.
    Open { assignee: bool },
}

/// Why a request is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// 401: only the committee changes a status.
    StatusIsCommittees,
    /// 401: only the committee assigns.
    AssigneeIsCommittees,
    /// 404: there is no curation to update.
    NoCuration,
    /// 400: the newest curation is still pending.
    Ongoing,
    /// 400: the item was never submitted.
    NeverCurated,
    /// 400: an item's curation has no assignee.
    ItemAssignee,
}

/// The table: what `actor` may do to a curation whose newest row is `from`.
pub fn decide(
    curation: Curation,
    from: Option<Status>,
    request: Request,
    actor: Actor,
) -> Result<Change, Refusal> {
    let creator = actor == Actor::Creator;
    match (curation, request) {
        (_, Request::Update { assignee: true, .. }) if creator => {
            Err(Refusal::AssigneeIsCommittees)
        }
        (Curation::Item, Request::Update { assignee: true, .. }) => Err(Refusal::ItemAssignee),
        (
            Curation::Collection,
            Request::Update {
                status: Some(_), ..
            },
        ) if creator => Err(Refusal::StatusIsCommittees),
        (
            Curation::Item,
            Request::Update {
                status: Some(Status::Approved | Status::Rejected),
                ..
            },
        ) if creator => Err(Refusal::StatusIsCommittees),
        (_, Request::Update { .. }) if from.is_none() => Err(Refusal::NoCuration),
        (
            Curation::Item,
            Request::Update {
                status: Some(Status::Pending),
                ..
            },
        ) if creator && from != Some(Status::Pending) => Err(Refusal::StatusIsCommittees),
        (_, Request::Update { status, assignee }) => Ok(Change::Update { status, assignee }),
        (Curation::Item, Request::Open { .. }) if from.is_none() => Err(Refusal::NeverCurated),
        (_, Request::Open { .. }) if from == Some(Status::Pending) => Err(Refusal::Ongoing),
        (Curation::Collection, Request::Open { assignee: true }) if creator => {
            Err(Refusal::AssigneeIsCommittees)
        }
        (Curation::Item, Request::Open { .. }) => Ok(Change::Open { assignee: false }),
        (Curation::Collection, Request::Open { assignee }) => Ok(Change::Open { assignee }),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use Actor::{Committee, Creator};
    use Curation::{Collection, Item};
    use Refusal::*;
    use Status::{Approved, Pending, Rejected};

    const FROM: [Option<Status>; 4] = [None, Some(Pending), Some(Approved), Some(Rejected)];

    fn update(status: Option<Status>, assignee: bool) -> Request {
        Request::Update { status, assignee }
    }

    /// Every cell of the table, written out: no cell is derived from
    /// another, and none from [`decide`].
    pub(crate) fn expected(
        curation: Curation,
        from: Option<Status>,
        request: Request,
        actor: Actor,
    ) -> Result<Change, Refusal> {
        let decided = matches!(from, Some(Approved | Rejected));
        match (curation, request, actor) {
            (_, Request::Update { assignee: true, .. }, Creator) => Err(AssigneeIsCommittees),
            (Item, Request::Update { assignee: true, .. }, Committee) => Err(ItemAssignee),
            (
                Collection,
                Request::Update {
                    status: Some(_), ..
                },
                Creator,
            ) => Err(StatusIsCommittees),
            (
                Item,
                Request::Update {
                    status: Some(Approved | Rejected),
                    ..
                },
                Creator,
            ) => Err(StatusIsCommittees),
            (_, Request::Update { .. }, _) if from.is_none() => Err(NoCuration),
            (
                Item,
                Request::Update {
                    status: Some(Pending),
                    ..
                },
                Creator,
            ) if decided => Err(StatusIsCommittees),
            (_, Request::Update { status, assignee }, _) => Ok(Change::Update { status, assignee }),
            (Item, Request::Open { .. }, _) if from.is_none() => Err(NeverCurated),
            (_, Request::Open { .. }, _) if from == Some(Pending) => Err(Ongoing),
            (Collection, Request::Open { assignee: true }, Creator) => Err(AssigneeIsCommittees),
            (Item, Request::Open { .. }, _) => Ok(Change::Open { assignee: false }),
            (Collection, Request::Open { assignee }, _) => Ok(Change::Open { assignee }),
        }
    }

    #[test]
    fn every_cell_of_the_transition_table_answers_as_written() {
        let mut requests = vec![
            Request::Open { assignee: false },
            Request::Open { assignee: true },
        ];
        for status in [None, Some(Pending), Some(Approved), Some(Rejected)] {
            for assignee in [false, true] {
                if status.is_some() || assignee {
                    requests.push(update(status, assignee));
                }
            }
        }
        let mut cells = 0;
        for curation in [Collection, Item] {
            for from in FROM {
                for &request in &requests {
                    for actor in [Committee, Creator] {
                        assert_eq!(
                            decide(curation, from, request, actor),
                            expected(curation, from, request, actor),
                            "{curation:?} from {from:?} {request:?} by {actor:?}"
                        );
                        cells += 1;
                    }
                }
            }
        }
        assert_eq!(cells, 2 * 4 * 9 * 2);
    }

    #[test]
    fn a_creator_never_changes_a_decided_curation() {
        for curation in [Collection, Item] {
            for from in [Some(Approved), Some(Rejected)] {
                for status in Status::ALL {
                    assert!(
                        decide(curation, from, update(Some(status), false), Creator).is_err(),
                        "{curation:?} from {from:?} to {status:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_creator_refreshes_a_pending_item_and_reopens_a_decided_one_by_a_new_request() {
        assert_eq!(
            decide(Item, Some(Pending), update(Some(Pending), false), Creator),
            Ok(Change::Update {
                status: Some(Pending),
                assignee: false
            })
        );
        for from in [Some(Approved), Some(Rejected)] {
            assert_eq!(
                decide(Item, from, Request::Open { assignee: false }, Creator),
                Ok(Change::Open { assignee: false })
            );
            assert_eq!(
                decide(Collection, from, Request::Open { assignee: false }, Creator),
                Ok(Change::Open { assignee: false })
            );
        }
    }
}
