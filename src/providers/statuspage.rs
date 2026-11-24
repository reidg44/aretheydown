//! Atlassian Statuspage (`/api/v2/summary.json`), used by GitHub, Cloudflare, and many others.
//! Recently resolved incidents come from a second feed, `/api/v2/incidents.json`.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::model::{
    ComponentNode, Incident, IncidentKind, Level, PastIncident, PlatformStatus, Update, name_key,
    non_blank, resolved_recently, sort_newest_end_first,
};

#[derive(Deserialize)]
struct Summary {
    status: Indicator,
    /// Every component on the page, groups included.
    #[serde(default)]
    components: Vec<RawComponent>,
    /// Statuspage lists only unresolved incidents here.
    #[serde(default)]
    incidents: Vec<RawIncident>,
    /// Upcoming and in-progress maintenance; only in-progress ones are shown.
    #[serde(default)]
    scheduled_maintenances: Vec<RawMaintenance>,
}

/// `incidents.json`: the page's recent incidents, resolved and unresolved,
/// newest first.
#[derive(Deserialize)]
struct IncidentList {
    page: Option<RawPage>,
    #[serde(default)]
    incidents: Vec<RawIncident>,
}

#[derive(Deserialize)]
struct RawPage {
    /// The page root, e.g. `https://status.openai.com/`.
    url: Option<String>,
}

#[derive(Deserialize)]
struct Indicator {
    indicator: String,
    description: String,
}

#[derive(Deserialize)]
struct RawIncident {
    id: Option<String>,
    name: String,
    impact: Option<String>,
    started_at: Option<DateTime<Utc>>,
    created_at: Option<DateTime<Utc>>,
    /// Set once the incident is resolved (`incidents.json` only).
    resolved_at: Option<DateTime<Utc>>,
    shortlink: Option<String>,
    #[serde(default)]
    components: Vec<RawComponent>,
    /// `investigating`, `identified`, `monitoring`, …
    status: Option<String>,
    #[serde(default)]
    incident_updates: Vec<RawUpdate>,
}

#[derive(Deserialize)]
struct RawMaintenance {
    name: String,
    status: String,
    started_at: Option<DateTime<Utc>>,
    scheduled_for: Option<DateTime<Utc>>,
    shortlink: Option<String>,
    #[serde(default)]
    components: Vec<RawComponent>,
    #[serde(default)]
    incident_updates: Vec<RawUpdate>,
}

#[derive(Deserialize)]
struct RawUpdate {
    body: Option<String>,
    created_at: Option<DateTime<Utc>>,
}

/// The update with the newest `created_at` that has any text. The order is
/// not relied on; the feed lists newest first, which settles ties.
fn latest_update(updates: &[RawUpdate]) -> Option<Update> {
    Update::latest(
        updates
            .iter()
            .filter_map(|u| Update::new(u.body.as_deref(), u.created_at)),
    )
}

impl RawMaintenance {
    /// `in_progress` and `verifying` mean the work is happening now.
    fn is_active(&self) -> bool {
        matches!(self.status.as_str(), "in_progress" | "verifying")
    }
}

#[derive(Deserialize, Clone)]
struct RawComponent {
    name: String,
    id: Option<String>,
    status: Option<String>,
    group_id: Option<String>,
    /// `true` for a group row, whose status sums up its components.
    #[serde(default)]
    group: bool,
}

fn component_names(components: &[RawComponent]) -> Vec<String> {
    components.iter().map(|c| c.name.clone()).collect()
}

/// Map a Statuspage `status.indicator` to a [`Level`].
///
/// `maintenance` (scheduled work in progress) counts as `Degraded`; any value
/// Statuspage adds later counts as `Unknown`.
#[must_use]
pub fn level_for_indicator(indicator: &str) -> Level {
    match indicator {
        "none" => Level::Operational,
        "minor" | "maintenance" => Level::Degraded,
        "major" | "critical" => Level::Outage,
        _ => Level::Unknown,
    }
}

/// Map a component's `status` to a [`Level`]; unknown values are `Unknown`.
fn level_for_component(status: &str) -> Level {
    match status {
        "operational" => Level::Operational,
        "degraded_performance" | "partial_outage" | "under_maintenance" => Level::Degraded,
        "major_outage" => Level::Outage,
        _ => Level::Unknown,
    }
}

/// Map an incident's `impact` to a [`Level`]; unknown values are `Unknown`.
fn level_for_impact(impact: &str) -> Level {
    match impact {
        "none" => Level::Operational,
        "minor" => Level::Degraded,
        "major" | "critical" => Level::Outage,
        _ => Level::Unknown,
    }
}

/// Parse a `summary.json` body into a [`PlatformStatus`].
///
/// # Errors
///
/// Fails when the body is not a Statuspage summary.
pub fn parse(name: &str, body: &[u8], fetched_at: DateTime<Utc>) -> Result<PlatformStatus> {
    parse_watching(name, body, fetched_at, None)
}

/// [`parse`], counting only the components in `watch` when given (the
/// `[components]` config). See [`Watch`] for how names match.
///
/// With a watch, the level is the worst of the watched components' statuses,
/// the counted incidents' impacts, and Degraded for each counted in-progress
/// maintenance; the vendor's own indicator is ignored. An incident or
/// maintenance counts when it lists no components or touches a watched one.
/// The description is built here: which watched components are not
/// operational, how many incidents elsewhere were left out, and any watched
/// name the feed does not have.
///
/// # Errors
///
/// Fails when the body is not a Statuspage summary.
pub fn parse_watching(
    name: &str,
    body: &[u8],
    fetched_at: DateTime<Utc>,
    watch: Option<&[String]>,
) -> Result<PlatformStatus> {
    let summary: Summary =
        serde_json::from_slice(body).context("response is not a Statuspage summary")?;
    Ok(summary_status(name, summary, fetched_at, watch))
}

/// [`parse_watching`] plus the resolved list from an `incidents.json` body
/// (see [`parse_resolved`]). `incidents` is `None` when that request failed;
/// then, or when the body does not parse, `resolved` stays empty and the rest
/// of the status is exactly what `summary` gives. A group name in `watch`
/// matches through the summary's components, which `incidents.json` lacks.
///
/// # Errors
///
/// Fails when `summary` is not a Statuspage summary.
pub fn parse_with_resolved(
    name: &str,
    summary: &[u8],
    incidents: Option<&[u8]>,
    fetched_at: DateTime<Utc>,
    watch: Option<&[String]>,
) -> Result<PlatformStatus> {
    let summary: Summary =
        serde_json::from_slice(summary).context("response is not a Statuspage summary")?;
    let page_components = summary.components.clone();
    let mut status = summary_status(name, summary, fetched_at, watch);
    if let Some(body) = incidents {
        status.resolved =
            resolved_from(body, fetched_at, watch, &page_components).unwrap_or_default();
    }
    Ok(status)
}

/// The incidents in an `incidents.json` body resolved within
/// [`RESOLVED_WINDOW`](crate::model::RESOLVED_WINDOW) of `fetched_at`, newest
/// `resolved_at` first. With `watch`, only incidents that list no components
/// or touch a watched one count (group names match only through components
/// the feed lists; [`parse_with_resolved`] knows every group). Each links to
/// its `shortlink`, else to `<page root>/incidents/<id>`.
///
/// # Errors
///
/// Fails when the body is not a Statuspage incident list.
pub fn parse_resolved(
    body: &[u8],
    fetched_at: DateTime<Utc>,
    watch: Option<&[String]>,
) -> Result<Vec<Incident>> {
    resolved_from(body, fetched_at, watch, &[])
}

/// [`parse_resolved`], with `page_components` (the summary's, groups
/// included) added to what the incidents list so group names match.
fn resolved_from(
    body: &[u8],
    fetched_at: DateTime<Utc>,
    watch: Option<&[String]>,
    page_components: &[RawComponent],
) -> Result<Vec<Incident>> {
    let mut resolved: Vec<Incident> = listed_incidents(body, watch, page_components)?
        .map(|(_, incident)| incident)
        .filter(|i| {
            i.ended_at
                .is_some_and(|at| resolved_recently(at, fetched_at))
        })
        .collect();
    sort_newest_end_first(&mut resolved);
    Ok(resolved)
}

/// The incidents in an `incidents.json` body, in feed order, each with its
/// `impact` as a level (a missing one is Unknown, as for active incidents)
/// and linked to its `shortlink`, else `<page root>/incidents/<id>`. With
/// `watch`, only incidents that list no components or touch a watched one
/// are kept; `page_components` (a summary's, groups included) are added to
/// the incidents' own so group names match.
fn listed_incidents(
    body: &[u8],
    watch: Option<&[String]>,
    page_components: &[RawComponent],
) -> Result<impl Iterator<Item = (Level, Incident)>> {
    let list: IncidentList =
        serde_json::from_slice(body).context("response is not a Statuspage incident list")?;
    let root = list
        .page
        .and_then(|page| page.url)
        .map(|url| url.trim_end_matches('/').to_owned());
    let mut incidents = list.incidents;
    if let Some(watch) = watch {
        let components = known_components(page_components, &incidents, &[]);
        let watch = Watch::new(watch, &components);
        incidents.retain(|i| watch.counts(&i.components));
    }
    Ok(incidents.into_iter().map(move |raw| {
        let level = raw
            .impact
            .as_deref()
            .map_or(Level::Unknown, level_for_impact);
        (level, linked_incident(raw, root.as_deref()))
    }))
}

/// Every incident in an `incidents.json` body, resolved or not, for history
/// backfill ([`crate::history::backfill`]), each with its `impact` as a level
/// (a missing one is Unknown, as for active incidents). With `watch`, only
/// incidents that list no components or touch a watched one count; a group
/// name matches through `summary`'s components when given (the incident list
/// has no group rows). Links as in [`parse_resolved`].
///
/// # Errors
///
/// Fails when `body` is not a Statuspage incident list, or `summary` (when
/// given) is not a summary.
pub fn parse_past(
    body: &[u8],
    summary: Option<&[u8]>,
    watch: Option<&[String]>,
) -> Result<Vec<PastIncident>> {
    let page_components = match summary {
        Some(summary) => {
            serde_json::from_slice::<Summary>(summary)
                .context("response is not a Statuspage summary")?
                .components
        }
        None => Vec::new(),
    };
    Ok(listed_incidents(body, watch, &page_components)?
        .map(|(level, incident)| PastIncident { level, incident })
        .collect())
}

/// [`incident`], linking to `<root>/incidents/<id>` when there is no shortlink.
fn linked_incident(raw: RawIncident, root: Option<&str>) -> Incident {
    let fallback = match (root, &raw.id) {
        (Some(root), Some(id)) => Some(format!("{root}/incidents/{id}")),
        _ => None,
    };
    let mut incident = incident(raw);
    incident.url = incident.url.or(fallback);
    incident
}

/// The status a parsed summary gives, as [`parse_watching`] describes.
fn summary_status(
    name: &str,
    summary: Summary,
    fetched_at: DateTime<Utc>,
    watch: Option<&[String]>,
) -> PlatformStatus {
    let maintenances: Vec<RawMaintenance> = summary
        .scheduled_maintenances
        .into_iter()
        .filter(RawMaintenance::is_active)
        .collect();
    let components = known_components(&summary.components, &summary.incidents, &maintenances);
    let tree = component_tree(&components);
    let status =
        |level, description, incidents: Vec<RawIncident>, maintenances: Vec<RawMaintenance>| {
            PlatformStatus {
                name: name.to_owned(),
                level,
                description,
                incidents: incidents
                    .into_iter()
                    .map(incident)
                    .chain(maintenances.into_iter().map(maintenance))
                    .collect(),
                resolved: Vec::new(),
                fetched_at,
                error: None,
                probe: None,
                regions: Vec::new(),
                component_tree: tree.clone(),
            }
        };
    let Some(watch) = watch else {
        return status(
            level_for_indicator(&summary.status.indicator),
            summary.status.description,
            summary.incidents,
            maintenances,
        );
    };

    let watch = Watch::new(watch, &components);

    let watched: Vec<&RawComponent> = components
        .iter()
        .filter(|c| !c.group && watch.matches(c))
        .collect();
    let troubled: Vec<&str> = watched
        .iter()
        .filter(|c| component_level(c) != Level::Operational)
        .map(|c| c.name.as_str())
        .collect();

    let (incidents, other_incidents): (Vec<RawIncident>, Vec<RawIncident>) = summary
        .incidents
        .into_iter()
        .partition(|i| watch.counts(&i.components));
    let (maintenances, other_maintenances): (Vec<RawMaintenance>, Vec<RawMaintenance>) =
        maintenances
            .into_iter()
            .partition(|m| watch.counts(&m.components));

    let level = Level::worst(
        watched
            .iter()
            .map(|c| component_level(c))
            .chain(
                incidents
                    .iter()
                    .map(|i| i.impact.as_deref().map_or(Level::Unknown, level_for_impact)),
            )
            .chain(maintenances.iter().map(|_| Level::Degraded)),
    );

    let left_out = other_incidents.len() + other_maintenances.len();
    let description = describe(&watched, &troubled, left_out, &watch.missing(&components));
    status(level, description, incidents, maintenances)
}

/// The filtered description: which watched components are not operational,
/// then how many incidents (maintenance included) were left out, then any
/// watched name the feed does not have.
fn describe(
    watched: &[&RawComponent],
    troubled: &[&str],
    left_out: usize,
    missing: &[&str],
) -> String {
    let mut parts = vec![if troubled.is_empty() {
        match watched.len() {
            0 => "No watched components in the feed".to_owned(),
            1 => "1 watched component operational".to_owned(),
            n => format!("All {n} watched components operational"),
        }
    } else {
        format!(
            "{} of {} watched components not operational: {}",
            troubled.len(),
            watched.len(),
            name_list(troubled)
        )
    }];
    match left_out {
        0 => {}
        1 => parts.push("(1 incident on other components not shown)".to_owned()),
        n => parts.push(format!("({n} incidents on other components not shown)")),
    }
    if !missing.is_empty() {
        let quoted: Vec<String> = missing.iter().map(|n| format!("{n:?}")).collect();
        parts.push(format!("(not in feed: {})", quoted.join(", ")));
    }
    parts.join(" ")
}

fn incident(raw: RawIncident) -> Incident {
    Incident {
        kind: IncidentKind::Incident,
        title: raw.name,
        components: component_names(&raw.components),
        started_at: raw.started_at.or(raw.created_at),
        ended_at: raw.resolved_at,
        url: raw.shortlink,
        stage: non_blank(raw.status.as_deref()),
        latest_update: latest_update(&raw.incident_updates),
    }
}

fn maintenance(raw: RawMaintenance) -> Incident {
    Incident {
        kind: IncidentKind::Maintenance,
        title: raw.name,
        components: component_names(&raw.components),
        started_at: raw.started_at.or(raw.scheduled_for),
        ended_at: None,
        url: raw.shortlink,
        stage: non_blank(Some(&raw.status)),
        latest_update: latest_update(&raw.incident_updates),
    }
}

/// A component's level; a missing status is Unknown.
fn component_level(component: &RawComponent) -> Level {
    component
        .status
        .as_deref()
        .map_or(Level::Unknown, level_for_component)
}

/// At most this many names are spelled out in the description.
const LISTED_NAMES: usize = 3;

/// `A, B, C` or `A, B, C and 4 more`.
fn name_list(names: &[&str]) -> String {
    let shown = names[..names.len().min(LISTED_NAMES)].join(", ");
    match names.len().saturating_sub(LISTED_NAMES) {
        0 => shown,
        more => format!("{shown} and {more} more"),
    }
}

/// Every component the feed mentions: the page's list first, then any that
/// only appear on an incident or in-progress maintenance (a trimmed or
/// paginated feed may leave them out of `components`). Deduplicated by id,
/// else by name.
fn known_components(
    page: &[RawComponent],
    incidents: &[RawIncident],
    maintenances: &[RawMaintenance],
) -> Vec<RawComponent> {
    let key = |c: &RawComponent| c.id.clone().unwrap_or_else(|| name_key(&c.name));
    let mut seen = HashSet::new();
    page.iter()
        .chain(incidents.iter().flat_map(|i| &i.components))
        .chain(maintenances.iter().flat_map(|m| &m.components))
        .filter(|c| seen.insert(key(c)))
        .cloned()
        .collect()
}

/// The group `component` sits in, when that group is one of `group_ids`.
fn in_group<'a>(component: &'a RawComponent, group_ids: &HashSet<&str>) -> Option<&'a str> {
    component
        .group_id
        .as_deref()
        .filter(|id| group_ids.contains(id))
}

/// `components` (from [`known_components`]) as the picker's tree: each group
/// row where it sits, holding the components whose `group_id` is its `id`;
/// every other component at the top level where it sits. Blank names are
/// left out.
fn component_tree(components: &[RawComponent]) -> Vec<ComponentNode> {
    let group_ids: HashSet<&str> = components
        .iter()
        .filter(|c| c.group)
        .filter_map(|c| c.id.as_deref())
        .collect();
    let mut members: HashMap<&str, Vec<String>> = HashMap::new();
    for component in components.iter().filter(|c| !c.group) {
        if let Some(id) = in_group(component, &group_ids)
            && !component.name.trim().is_empty()
        {
            members.entry(id).or_default().push(component.name.clone());
        }
    }
    components
        .iter()
        .filter(|c| !c.name.trim().is_empty())
        .filter_map(|c| {
            if c.group {
                let components =
                    c.id.as_deref()
                        .and_then(|id| members.remove(id))
                        .unwrap_or_default();
                Some(ComponentNode::Group {
                    name: c.name.clone(),
                    components,
                })
            } else if in_group(c, &group_ids).is_some() {
                None
            } else {
                Some(ComponentNode::Component(c.name.clone()))
            }
        })
        .collect()
}

/// The watched names, and every component's name by id so a component's
/// `group_id` finds its group's name (a group name covers its components).
struct Watch<'a> {
    names: Vec<String>,
    originals: &'a [String],
    names_by_id: HashMap<String, String>,
}

impl<'a> Watch<'a> {
    fn new(names: &'a [String], components: &[RawComponent]) -> Self {
        let names_by_id = components
            .iter()
            .filter_map(|c| Some((c.id.clone()?, name_key(&c.name))))
            .collect();
        Self {
            names: names.iter().map(|n| name_key(n)).collect(),
            originals: names,
            names_by_id,
        }
    }

    /// A component matches when its own name or its group's name is watched.
    fn matches(&self, component: &RawComponent) -> bool {
        let group = component
            .group_id
            .as_ref()
            .and_then(|id| self.names_by_id.get(id));
        self.names
            .iter()
            .any(|n| *n == name_key(&component.name) || Some(n) == group)
    }

    /// An incident or maintenance counts when it lists no components or any
    /// of them matches.
    fn counts(&self, components: &[RawComponent]) -> bool {
        components.is_empty() || components.iter().any(|c| self.matches(c))
    }

    /// Watched names (trimmed, as written) that name no component or group.
    fn missing(&self, components: &[RawComponent]) -> Vec<&'a str> {
        self.names
            .iter()
            .zip(self.originals)
            .filter(|(n, _)| !components.iter().any(|c| name_key(&c.name) == **n))
            .map(|(_, original)| original.trim())
            .collect()
    }
}
