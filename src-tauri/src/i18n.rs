//! User-facing strings in Chinese and English. `t(lang, key)` then `.replace("{x}", ..)` via `tf`.

const TABLE: &[(&str, &str, &str)] = &[
    // severity
    ("sev.high", "高", "High"),
    ("sev.medium", "中", "Medium"),
    ("sev.low", "低", "Low"),
    ("sev.nit", "nit", "Nit"),
    ("sev_icon.high", "🔴 高", "🔴 High"),
    ("sev_icon.medium", "🟠 中", "🟠 Medium"),
    ("sev_icon.low", "🟡 低", "🟡 Low"),
    ("sev_icon.nit", "⚪ nit", "⚪ Nit"),
    // why the gate did not approve
    ("reason.unable", "审查未能完成：{detail}", "review could not finish: {detail}"),
    ("reason.high", "有高风险问题", "high-severity issue found"),
    ("reason.introduced_medium", "有 PR 引入的中风险问题", "medium-severity issue introduced by this PR"),
    ("reason.model_not_recommended", "审查结论不建议 approve", "reviewer did not recommend approval"),
    ("reason.ci_failed", "CI 失败", "CI failed"),
    ("reason.ci_pending", "CI 还没跑完", "CI still running"),
    ("reason.ci_none_wait", "还没有 CI 结果", "no CI result yet"),
    ("reason.closed_while_waiting", "等 CI 期间 PR 已合并或关闭，没有再发评审", "the PR was merged or closed while waiting for CI; no review was posted"),
    ("reason.own_pr", "这是你自己的 PR，GitHub 不允许 approve", "this is your own PR; GitHub does not allow approving it"),
    ("reason.not_open", "PR 不是打开的非草稿状态", "PR is not open or is a draft"),
    ("reason.approve_disabled", "已配置为只评论，不自动 approve", "auto-approval is turned off (approve.mode = never)"),
    ("reason.approve_repo", "这个仓库不在自动 approve 名单里", "this repository is not on the auto-approve list"),
    ("reason.engine_no_approve", "这个审查引擎隔离较弱，按设置不自动 approve", "this review engine is not isolated enough to approve (see its settings)"),
    ("reason.approve_author", "这个作者不在自动 approve 名单里", "this author is not on the auto-approve list"),
    // GitHub review body
    ("review.approve", "**结论：Approve。** {summary}", "**Verdict: Approve.** {summary}"),
    ("review.comment", "**结论：只评论，未 approve。** {summary}", "**Verdict: comments only, not approved.** {summary}"),
    ("review.blocked_by", "卡在：{reasons}", "Blocked by: {reasons}"),
    (
        "review.legend",
        "问题清单（🔴 高会阻塞 approve；🟠 中建议本 PR 修；🟡 低可排后；⚪ nit 顺手改）：",
        "Findings (🔴 High blocks approval; 🟠 Medium should be fixed in this PR; 🟡 Low can wait; ⚪ Nit is optional):",
    ),
    ("review.old_issue", " · 老问题", " · pre-existing"),
    ("review.outside_diff", "（`{loc}`，不在 diff 内）", " (`{loc}`, outside the diff)"),
    (
        "review.footer",
        "<sub>由 Nod 用 Claude 自动审查；approve 由固定规则决定：无高风险、无 PR 引入的中风险、CI 未失败、非草稿。</sub>",
        "<sub>Automated review by Nod using Claude. Approval is decided by fixed rules: no high-severity issues, no medium-severity issues introduced by the PR, CI not failing, not a draft.</sub>",
    ),
    (
        "review.direct",
        "**结论：Approve。** 由本人直接通过（未等待自动审查完成）。",
        "**Verdict: Approve.** Approved directly by the reviewer (without waiting for the automated review).",
    ),
    (
        "review.dismiss_msg",
        "Nod：新提交审查未通过，撤销之前的 approve",
        "nod: a newer commit did not pass review; withdrawing the earlier approval",
    ),
    ("review.no_body", "（无）", "(none)"),
    // notes
    ("note.withdrawn", "已撤销之前的 {n} 个 approve。", "Withdrew {n} earlier approval(s)."),
    (
        "note.withdraw_failed",
        "⚠️ 之前的 approve 没能撤销，仍然有效。请到 PR 上看看是否需要处理（有的平台只允许管理员撤销）。",
        "⚠️ Could not withdraw the earlier approval; it is still in effect. Check the PR (some hosts only let administrators withdraw approvals).",
    ),
    (
        "note.withdraw_error",
        "⚠️ 检查或撤销之前的 approve 时出错，之前的 approve 可能仍然有效，请到 PR 上确认。",
        "⚠️ Error while checking earlier approvals; an earlier approval may still be in effect.",
    ),
    (
        "note.stale_approve",
        "⚠️ 自动审查之前 approve 过这个 PR 的旧提交，那个 approve 仍然有效，请人工确认新提交。",
        "⚠️ Nod approved an earlier commit of this PR and that approval is still in effect. Check the new commits yourself.",
    ),
    ("note.review_failed", "自动审查失败 {n} 次，没有 approve。原因：{err}", "Automated review failed {n} time(s); not approved. Error: {err}"),
    (
        "note.post_failed",
        "审查有结论，但发到 GitHub 失败 {n} 次，没有 approve。原因：{err}",
        "Review finished but posting to GitHub failed {n} time(s); not approved. Error: {err}",
    ),
    (
        "note.secret_blocked",
        "审查内容疑似包含凭据（{kinds}），已拦截，没有发到 GitHub，也没有 approve。",
        "The review text looked like it contained credentials ({kinds}); it was not posted and the PR was not approved.",
    ),
    ("note.system_failed", "自动审查整轮失败，没有审查任何 PR。原因：{err}", "A whole polling round failed; no PR was reviewed. Error: {err}"),
    // direct approve feedback
    ("direct.working", "⏳ 正在直接通过…", "⏳ Approving…"),
    ("direct.closed", "⚠️ PR 已关闭或合并，没有 approve。", "⚠️ The PR is closed or merged; not approved."),
    ("direct.own_pr", "⚠️ 这是你自己的 PR，GitHub 不允许 approve。", "⚠️ This is your own PR; GitHub does not allow approving it."),
    (
        "direct.moved",
        "⚠️ 你点按钮时 PR 已有新提交 {sha}，没有 approve。卡片已刷新，请确认后再点。",
        "⚠️ The PR got a new commit {sha} before you clicked; not approved. The card has been refreshed — check it and click again.",
    ),
    ("direct.failed", "⚠️ 直接通过失败：{err}", "⚠️ Direct approval failed: {err}"),
    ("direct.not_you", "只有本人可以操作", "Only the configured user can do this"),
    ("direct.unknown_pr", "找不到这个 PR 的记录", "No record of this PR"),
    // card
    ("status.merged", "已合并", "Merged"),
    ("status.closed", "已关闭", "Closed"),
    ("status.direct", "已直接通过", "Approved by you"),
    ("status.queued", "排队中", "Queued"),
    ("status.reviewing", "审查中", "Reviewing"),
    ("status.awaiting_ci", "等 CI", "Waiting for CI"),
    ("status.approved", "已 approve", "Approved"),
    ("status.ready", "可以通过", "Looks good"),
    ("status.issues", "有问题", "Has issues"),
    ("status.manual", "需人工处理", "Needs attention"),
    ("status.out_of_scope", "不在审查范围", "Out of scope"),
    ("card.pr", "PR", "PR"),
    ("card.decision", "PR 评审状态", "PR review status"),
    ("card.ci", "CI", "CI"),
    ("card.size", "改动", "Size"),
    ("card.open", "打开", "Open"),
    ("card.draft", "草稿", "Draft"),
    ("card.files", "{n} 文件", "{n} files"),
    ("card.commit", "提交 {sha}", "commit {sha}"),
    ("decision.APPROVED", "已批准", "Approved"),
    ("decision.CHANGES_REQUESTED", "要求修改", "Changes requested"),
    ("decision.REVIEW_REQUIRED", "待审查", "Review required"),
    ("ci.success", "通过", "Passing"),
    ("ci.failure", "失败", "Failing"),
    ("ci.pending", "运行中", "Running"),
    ("ci.none", "无", "None"),
    (
        "card.moved",
        "PR 有新提交 {new}，下面的结论针对 {old}，稍后自动重审。",
        "New commit {new} pushed; the verdict below is for {old}. It will be re-reviewed shortly.",
    ),
    (
        "card.queued",
        "**排队中，前面还有别的 PR**（{since} 开始），这张卡片会自动更新。",
        "**Queued behind another PR** (since {since}). This card updates automatically.",
    ),
    (
        "card.reviewing",
        "**正在审查**（{since} 开始），通常 1 分钟左右出结果，这张卡片会自动更新。",
        "**Reviewing** (since {since}). Usually about a minute; this card updates automatically.",
    ),
    ("card.direct", "**你在 {at} 直接通过了 {sha}**，已以你的身份 approve。", "**You approved {sha} at {at}.**"),
    ("card.aborted", " 进行中的自动审查已中止。", " The running automated review was stopped."),
    (
        "card.awaiting_ci",
        "**结论：没有阻塞问题。** {summary}\n等 CI 跑完后自动 approve。",
        "**Verdict: no blocking issues.** {summary}\nWill approve once CI finishes.",
    ),
    ("card.verdict", "**结论：** {summary}", "**Verdict:** {summary}"),
    ("card.not_approved", "未自动 approve：", "Not auto-approved: "),
    ("card.n_issues", "**{n} 个问题**", "**{n} finding(s)**"),
    ("card.more", "另有 {n} 条，见 GitHub 评审。", "{n} more on GitHub."),
    ("card.btn_direct", "直接通过", "Approve now"),
    ("card.btn_github", "在 GitHub 查看", "Open on GitHub"),
    ("card.btn_review", "评审意见", "View review"),
    ("card.confirm_title", "直接通过这个 PR？", "Approve this PR now?"),
    (
        "card.confirm_text",
        "以你的身份 approve {sha}，并中止正在进行的自动审查。",
        "Approve {sha} as you and stop the running automated review.",
    ),
    ("card.footer", "自动审查 · 更新于 {at}", "Nod · updated {at}"),
    ("card.alert_title", "⚠️ 自动审查整轮失败", "⚠️ Nod round failed"),
    ("card.test_title", "Nod 连通测试", "Nod connectivity test"),
    ("sys.approved", "已 approve", "Approved"),
    ("sys.commented", "已评论", "Commented"),
    ("sys.failed", "审查失败", "Review failed"),
    ("sys.direct", "已直接通过", "Approved directly"),
    ("robot.test_body", "能看到这条消息，说明机器人配置正确。", "If you can read this, the robot is configured correctly."),
    ("sys.test_body", "能看到这条通知，说明系统通知可用。", "If you can see this, desktop notifications work."),
    ("card.test_body", "收到这条消息说明飞书机器人配置正确。", "If you can read this, the Feishu bot is configured correctly."),
];

/// Reasons that say nothing about code quality: the code looks fine, the rules just do not allow auto-approval.
pub const NON_QUALITY_REASONS: [&str; 8] = [
    "engine_no_approve",
    "closed_while_waiting",
    "ci_pending",
    "ci_none_wait",
    "own_pr",
    "approve_disabled",
    "approve_repo",
    "approve_author",
];

pub fn t(lang: &str, key: &str) -> String {
    TABLE
        .iter()
        .find(|(k, _, _)| *k == key)
        .map(|(_, zh, en)| if lang == "en" { *en } else { *zh })
        .unwrap_or(key)
        .to_string()
}

/// Translate and fill `{name}` placeholders.
pub fn tf(lang: &str, key: &str, args: &[(&str, &str)]) -> String {
    let mut s = t(lang, key);
    for (k, v) in args {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

pub fn reason_text(lang: &str, reason: &str) -> String {
    match reason.split_once(':') {
        Some(("unable", detail)) => tf(lang, "reason.unable", &[("detail", detail)]),
        _ => t(lang, &format!("reason.{reason}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_unique_and_placeholders_match() {
        let mut seen = std::collections::HashSet::new();
        for (k, zh, en) in TABLE {
            assert!(seen.insert(*k), "duplicate {k}");
            let ph = |s: &str| {
                let mut v: Vec<String> = s
                    .split('{')
                    .skip(1)
                    .filter_map(|p| p.split_once('}').map(|x| x.0.to_string()))
                    .collect();
                v.sort();
                v
            };
            assert_eq!(ph(zh), ph(en), "{k}");
        }
    }

    #[test]
    fn reasons() {
        assert_eq!(reason_text("en", "ci_failed"), "CI failed");
        assert_eq!(
            reason_text("en", "unable:too big"),
            "review could not finish: too big"
        );
        assert_eq!(
            reason_text("zh-CN", "own_pr"),
            "这是你自己的 PR，GitHub 不允许 approve"
        );
    }
}
