use std::collections::BTreeSet;

use sigil_kernel::{
    DisclosurePresentationError, EgressDataCategory, EgressDisclosureKind, EgressNetworkRoute,
    PreEgressDisclosure,
};

use super::{AppState, EgressDisclosureReceiptTx};

pub(crate) const EGRESS_DISCLOSURE_HEIGHT: u16 = 5;

/// Safe data rendered in the TUI before a network disclosure may be acknowledged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EgressDisclosureCard {
    pub(crate) title: String,
    pub(crate) route: String,
    pub(crate) destination: String,
    pub(crate) data_categories: String,
    pub(crate) disclosure_count: usize,
    kind: EgressDisclosureKind,
    display_name: String,
    logical_destination: String,
    routes: Vec<EgressNetworkRoute>,
    categories: BTreeSet<EgressDataCategory>,
}

impl EgressDisclosureCard {
    fn from_disclosure(disclosure: &PreEgressDisclosure) -> Self {
        let mut card = Self {
            title: String::new(),
            route: String::new(),
            destination: String::new(),
            data_categories: String::new(),
            disclosure_count: 1,
            kind: disclosure.kind(),
            display_name: disclosure.display_name().to_owned(),
            logical_destination: disclosure.safe_logical_destination().to_owned(),
            routes: vec![disclosure.route()],
            categories: disclosure.data_categories().iter().copied().collect(),
        };
        card.refresh_labels();
        card
    }

    fn merge(mut self, next: Self) -> Self {
        if self.logical_destination != next.logical_destination {
            return next;
        }
        self.disclosure_count = self.disclosure_count.saturating_add(next.disclosure_count);
        if next.kind == EgressDisclosureKind::Query {
            self.kind = next.kind;
            self.display_name = next.display_name;
        }
        for route in next.routes {
            if !self.routes.contains(&route) {
                self.routes.push(route);
            }
        }
        self.categories.extend(next.categories);
        self.refresh_labels();
        self
    }

    fn refresh_labels(&mut self) {
        let kind = disclosure_kind_label(self.kind);
        self.title = if self.disclosure_count == 1 {
            format!("{} · {kind}", self.display_name)
        } else {
            format!(
                "{} · {kind} · {} disclosures",
                self.display_name, self.disclosure_count
            )
        };
        self.destination = format!("destination: {}", self.logical_destination);
        self.route = format!(
            "route: {}",
            self.routes
                .iter()
                .map(|route| route_label(*route))
                .collect::<Vec<_>>()
                .join(" + ")
        );
        self.data_categories = format!(
            "data: {}",
            self.categories
                .iter()
                .map(|category| data_category_label(*category))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}

#[derive(Debug)]
pub(super) struct PendingEgressDisclosure {
    disclosure: PreEgressDisclosure,
    receipt_tx: Option<EgressDisclosureReceiptTx>,
}

impl PendingEgressDisclosure {
    fn new(disclosure: PreEgressDisclosure, receipt_tx: EgressDisclosureReceiptTx) -> Self {
        Self {
            disclosure,
            receipt_tx: Some(receipt_tx),
        }
    }

    fn card(&self) -> EgressDisclosureCard {
        EgressDisclosureCard::from_disclosure(&self.disclosure)
    }
}

impl Drop for PendingEgressDisclosure {
    fn drop(&mut self) {
        if let Some(receipt_tx) = self.receipt_tx.take() {
            let _ = receipt_tx.send(Err(DisclosurePresentationError::SinkClosed));
        }
    }
}

impl AppState {
    pub(crate) fn egress_disclosure_reserved_rows(&self, available_height: u16) -> u16 {
        if available_height > EGRESS_DISCLOSURE_HEIGHT
            && self.active_egress_disclosure_card().is_some()
        {
            EGRESS_DISCLOSURE_HEIGHT
        } else {
            0
        }
    }

    pub(super) fn open_egress_disclosure(
        &mut self,
        disclosure: PreEgressDisclosure,
        receipt_tx: EgressDisclosureReceiptTx,
    ) {
        self.egress_disclosure
            .pending
            .push_back(PendingEgressDisclosure::new(disclosure, receipt_tx));
        self.last_notice = Some("network disclosure pending render".to_owned());
        self.push_event("network:disclosure", "pending frame render");
    }

    pub(crate) fn active_egress_disclosure_card(&self) -> Option<EgressDisclosureCard> {
        let pending = self
            .egress_disclosure
            .pending
            .front()
            .map(PendingEgressDisclosure::card);
        match (self.egress_disclosure.recent.clone(), pending) {
            (Some(recent), Some(pending)) => Some(recent.merge(pending)),
            (Some(recent), None) => Some(recent),
            (None, pending) => pending,
        }
    }

    pub(crate) fn begin_egress_disclosure_frame(&self) {
        self.egress_disclosure.rendered.set(false);
    }

    pub(crate) fn mark_egress_disclosure_rendered(&self) {
        self.egress_disclosure.rendered.set(true);
    }

    /// Acknowledges only the disclosure card that was included in a successfully completed frame.
    pub(crate) fn acknowledge_active_egress_disclosure_frame(&mut self) -> bool {
        if !self.egress_disclosure.rendered.replace(false) {
            return false;
        }
        let rendered_card = self.active_egress_disclosure_card();
        let Some(mut pending) = self.egress_disclosure.pending.pop_front() else {
            return false;
        };
        let result = pending
            .disclosure
            .presentation_receipt("tui-active-card-frame-v1");
        let sent = pending
            .receipt_tx
            .take()
            .is_some_and(|receipt_tx| receipt_tx.send(result).is_ok());
        if sent {
            self.egress_disclosure.recent = rendered_card;
            self.last_notice = Some("network disclosure rendered".to_owned());
            self.push_event("network:disclosure", "frame rendered");
        }
        sent
    }

    pub(super) fn clear_recent_egress_disclosure(&mut self) {
        self.egress_disclosure.recent = None;
        self.egress_disclosure.rendered.set(false);
    }
}

fn disclosure_kind_label(kind: EgressDisclosureKind) -> &'static str {
    match kind {
        EgressDisclosureKind::Transport => "connection disclosure",
        EgressDisclosureKind::Query => "query disclosure",
    }
}

fn route_label(route: EgressNetworkRoute) -> &'static str {
    match route {
        EgressNetworkRoute::Direct => "direct",
        EgressNetworkRoute::ProxyRemote => "environment proxy",
    }
}

fn data_category_label(category: EgressDataCategory) -> &'static str {
    match category {
        EgressDataCategory::SearchQuery => "search query",
        EgressDataCategory::ConnectionMetadata => "connection metadata",
        EgressDataCategory::WorkspaceRootUri => "workspace root URI",
        EgressDataCategory::InteractiveUserResponse => "interactive response",
    }
}
