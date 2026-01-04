---
name: github-project-management
description: |
  GitHub Projectで大規模Issueを体系的に管理します。
  親Issue・子Issue階層、優先度設定、自動化ワークフローを含む完全なセットアップを行います。
  「大規模な機能を実装する」「タスクを分割したい」「プロジェクト管理をセットアップしたい」などの場面で使用します。
---

# GitHub Project 大規模Issue管理 Skill

## 概要

大規模な機能実装やエピックをGitHub Projectで管理するためのワークフローです。

## 前提条件

- GitHub CLIがインストールされていること
- `gh auth login` で認証済みであること
- Projectsを使う場合は `gh auth refresh -s read:project,project` でスコープ追加が必要

## ワークフロー

### Step 1: 親Issueの作成

```bash
gh issue create --title "feat: [機能名]" --body "$(cat <<'EOF'
## 概要
[機能の説明]

## サブタスク
- [ ] #TBD サブタスク1
- [ ] #TBD サブタスク2

## 技術仕様
[詳細な仕様]
EOF
)"
```

### Step 2: サブIssueの作成

```bash
gh issue create --title "feat([scope]): [サブタスク名]" --body "$(cat <<'EOF'
## 親Issue
#[親Issue番号]

## タスク
[具体的なタスク内容]

## 変更ファイル
- `path/to/file`
EOF
)"
```

### Step 3: Sub-issues階層の設定（GraphQL API）

```bash
PARENT_ID=$(gh issue view [親Issue番号] --json id -q '.id')
CHILD_ID=$(gh issue view [子Issue番号] --json id -q '.id')

gh api graphql -f query="
mutation {
  addSubIssue(input: {issueId: \"$PARENT_ID\", subIssueId: \"$CHILD_ID\"}) {
    issue { title }
    subIssue { title }
  }
}"
```

### Step 4: GitHub Projectへの追加

```bash
# Projectを確認/作成
gh project list --owner [OWNER]

# なければ作成
gh project create --owner [OWNER] --title "[Project名]"

# IssueをProjectに追加
gh project item-add [PROJECT_NUMBER] --owner [OWNER] --url [ISSUE_URL]

# リポジトリにリンク
gh project link [PROJECT_NUMBER] --owner [OWNER] --repo [OWNER]/[REPO]
```

### Step 5: 優先度フィールドの追加

```bash
gh project field-create [PROJECT_NUMBER] --owner [OWNER] \
  --name "Priority" \
  --data-type "SINGLE_SELECT" \
  --single-select-options "High,Medium,Low"
```

### Step 6: 優先度の設定（GraphQL API）

```bash
# フィールドIDとオプションIDを取得
gh api graphql -f query='
query {
  user(login: "[OWNER]") {
    projectV2(number: [PROJECT_NUMBER]) {
      field(name: "Priority") {
        ... on ProjectV2SingleSelectField {
          id
          options { id name }
        }
      }
    }
  }
}'

# 優先度を設定
gh api graphql -f query="
mutation {
  updateProjectV2ItemFieldValue(input: {
    projectId: \"[PROJECT_ID]\"
    itemId: \"[ITEM_ID]\"
    fieldId: \"[FIELD_ID]\"
    value: {singleSelectOptionId: \"[OPTION_ID]\"}
  }) { projectV2Item { id } }
}"
```

## ビュー設定（WebUIで実施）

### 階層表示
1. Project → ビュー設定（▼）→ Group by → Parent issue

### Boardビュー
1. Project → + New view → Board

### Roadmapビュー
1. 日付フィールドを追加（Start Date, Target Date）
2. + New view → Roadmap

## 自動化ワークフロー

以下のワークフローが自動的に有効になります：
- Item added to project → Status: Todo
- Item closed → Status: Done
- Pull request merged → Status: Done
- Auto-add sub-issues to project

## ベストプラクティス

1. **命名規則**
   - 親Issue: `feat: [機能名]`
   - 子Issue: `feat([scope]): [タスク名]`

2. **優先度の基準**
   - High: 他のタスクの前提条件、ブロッカー
   - Medium: 通常の実装タスク
   - Low: 最後に実施、オプショナル

3. **親Issueの本文に含めるべき情報**
   - 概要
   - サブタスクへのリンク（自動更新される）
   - 技術仕様
   - 参照ファイル一覧
