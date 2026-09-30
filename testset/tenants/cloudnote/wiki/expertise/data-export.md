---
kind: expertise
title: Data export
aliases: [data export, export workspace, 数据导出, 导出笔记, backup, 备份]
keywords: [Markdown, HTML, PDF, export API, admin only, attachments, 导出格式, 附件]
summary: Admins can export a whole workspace as Markdown or HTML with attachments; members can export single notes as PDF; the export API allows one full export per hour.
about: [accounts:workspace]
sources: [admin-guide-v12.pdf]
updated: 2026-07-20
---

# Data export

## Workspace export

Only workspace admins can export everything. Formats:

- Markdown with attachments in a folder per note
- HTML with inline images

Large exports are prepared in the background; a download link is emailed and stays valid for 7
days.

## Single notes

Any member can export a note they can read as PDF, Markdown or Word.

## API

`POST /v1/exports` starts a full export. Limit: one full export per workspace per hour.
Comments and mentions are included as a separate JSON file, not inside the notes.
