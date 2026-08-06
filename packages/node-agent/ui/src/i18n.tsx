import { createContext, useContext, useEffect, useMemo, useState, type ReactNode } from 'react';
import { MESSAGES, type Locale as RustLocale } from '../../../../src/lib/i18n/catalog';

export type Locale = RustLocale;

const STORAGE_KEY = 'coding-tools.locale';
const SUPPORTED_LOCALES: readonly Locale[] = ['en', 'zh-TW', 'zh-CN', 'ja'];
const LOCALE_OPTIONS: ReadonlyArray<{ value: Locale; label: string }> = [
  { value: 'en', label: 'English' },
  { value: 'zh-TW', label: '繁體中文' },
  { value: 'zh-CN', label: '简体中文' },
  { value: 'ja', label: '日本語' }
];

const UI_MESSAGES: Record<string, readonly [string, string, string, string]> = {
  Dashboard: ['Dashboard', '總覽', '总览', 'ダッシュボード'],
  Settings: ['Settings', '設定', '设置', '設定'],
  Workspaces: ['Workspaces', '工具區', '工作区', 'ワークスペース'],
  Workspace: ['Workspace', '工具區', '工作區', 'ワークスペース'],
  Refresh: ['Refresh', '重新整理', '刷新', '更新'],
  'Refreshing…': ['Refreshing…', '更新中…', '刷新中…', '更新中…'],
  Running: ['Running', '運行中', '运行中', '実行中'],
  'Restart required': ['Restart required', '需要重新啟動', '需要重新启动', '再起動が必要'],
  'Install UI': ['Install UI', '安裝介面', '安装界面', 'UI をインストール'],
  Copy: ['Copy', '複製', '复制', 'コピー'],
  Copied: ['Copied', '已複製', '已复制', 'コピー済み'],
  Show: ['Show', '顯示', '显示', '表示'],
  Hide: ['Hide', '隱藏', '隐藏', '非表示'],
  Next: ['Next', '下一步', '下一步', '次へ'],
  'Not available': ['Not available', '尚未支援', '尚未支持', '未対応'],
  'Available now': ['Available now', '目前可用', '当前可用', '利用可能'],
  'Choose an existing workspace': ['Choose an existing workspace', '選擇現有工具區', '选择现有工作区', '既存のワークスペースを選択'],
  'Add or edit workspaces in Settings.': ['Add or edit workspaces in Settings.', '可在「設定」新增或修改工具區。', '可在“设置”中添加或修改工作区。', 'ワークスペースの追加や編集は「設定」で行えます。'],
  'Node Agent currently exposes MCP only.': ['Node Agent currently exposes MCP only.', 'Node Agent 目前僅提供 MCP 連線。', 'Node Agent 当前仅提供 MCP 连接。', 'Node Agent は現在 MCP 接続のみを提供します。'],
  'Public connection ID': ['Public connection ID', '公開連線 ID', '公网连接 ID', '公開接続 ID'],
  'Used in the public MCP URL. Keep letters, numbers, underscores, or hyphens.': ['Used in the public MCP URL. Keep letters, numbers, underscores, or hyphens.', '此值會出現在公開 MCP 網址中，僅可使用英數字、底線或連字號。', '此值会出现在公网 MCP 地址中，仅可使用字母、数字、下划线或连字符。', '公開 MCP URL に使用します。英数字、アンダースコア、ハイフンのみ使用できます。'],
  'Generated as a random UUID for registration. The server-assigned Client ID becomes authoritative after enrollment.': ['Generated as a random UUID for registration. The server-assigned Client ID becomes authoritative after enrollment.', '註冊用 Client ID 會以隨機 UUID 產生；Enrollment 完成後，以伺服器指派的 Client ID 為準。', '注册用 Client ID 会以随机 UUID 生成；Enrollment 完成后，以服务器分配的 Client ID 为准。', '登録用 Client ID はランダム UUID で生成され、Enrollment 完了後はサーバーが割り当てた Client ID が正となります。'],
  'Generate another connection ID': ['Generate another connection ID', '重新產生連線 ID', '重新生成连接 ID', '別の接続 ID を生成'],
  'Provisional MCP endpoint': ['Provisional MCP endpoint', '暫用 MCP 端點', '临时 MCP 端点', '仮 MCP エンドポイント'],
  'Enrollment replaces this provisional ID when the server assigns a different Client ID.': ['Enrollment replaces this provisional ID when the server assigns a different Client ID.', '若伺服器指派不同 Client ID，Enrollment 會自動替換此暫用 ID。', '如果服务器分配不同 Client ID，Enrollment 会自动替换此临时 ID。', 'サーバーが別の Client ID を割り当てた場合、Enrollment がこの仮 ID を自動的に置き換えます。'],
  'Generate another password': ['Generate another password', '重新產生密碼', '重新生成密码', '別のパスワードを生成'],
  'Save quick setup': ['Save quick setup', '儲存快速設定', '保存快速设置', 'クイック設定を保存'],
  'Saving…': ['Saving…', '儲存中…', '保存中…', '保存中…'],
  'Restart the Agent to apply the saved tunnel and OAuth settings.': ['Restart the Agent to apply the saved tunnel and OAuth settings.', '請重新啟動 Agent，套用已儲存的通道與 OAuth 設定。', '请重新启动 Agent，以应用已保存的隧道与 OAuth 设置。', '保存したトンネルと OAuth 設定を適用するには Agent を再起動してください。'],
  'Restart first. Enrollment will save the server-assigned Client ID as the final Public MCP endpoint.': ['Restart first. Enrollment will save the server-assigned Client ID as the final Public MCP endpoint.', '請先重新啟動。Enrollment 會把伺服器指派的 Client ID 儲存為正式 Public MCP 端點。', '请先重新启动。Enrollment 会把服务器分配的 Client ID 保存为正式 Public MCP 端点。', '先に再起動してください。Enrollment によりサーバー割り当ての Client ID が正式な Public MCP エンドポイントとして保存されます。'],
  'After restart, copy the final Public MCP endpoint from Dashboard and choose OAuth authentication.': ['After restart, copy the final Public MCP endpoint from Dashboard and choose OAuth authentication.', '重新啟動後，從 Dashboard 複製正式 Public MCP 端點，並選擇 OAuth 驗證。', '重新启动后，从 Dashboard 复制正式 Public MCP 端点，并选择 OAuth 验证。', '再起動後、Dashboard から正式な Public MCP エンドポイントをコピーし、OAuth 認証を選択します。'],
  'The final Public MCP endpoint is available on Dashboard after enrollment completes.': ['The final Public MCP endpoint is available on Dashboard after enrollment completes.', 'Enrollment 完成後，正式 Public MCP 端點會顯示在 Dashboard。', 'Enrollment 完成后，正式 Public MCP 端点会显示在 Dashboard。', 'Enrollment 完了後、正式な Public MCP エンドポイントが Dashboard に表示されます。'],
  'The password is shown only in this setup screen. Copy it before leaving.': ['The password is shown only in this setup screen. Copy it before leaving.', '此密碼只會顯示在本次設定畫面，離開前請先複製。', '此密码只会显示在本次设置页面，离开前请先复制。', 'このパスワードは今回の設定画面にのみ表示されます。移動前にコピーしてください。'],
  'Start another setup': ['Start another setup', '設定另一個專案', '设置另一个项目', '別の設定を開始'],
  'Open settings': ['Open settings', '開啟設定', '打开设置', '設定を開く'],
  'No public MCP endpoint is configured.': ['No public MCP endpoint is configured.', '尚未設定公開 MCP 端點。', '尚未设置公网 MCP 端点。', '公開 MCP エンドポイントが未設定です。'],
  'Use Quick setup to register this Agent with Built-in WSS.': ['Use Quick setup to register this Agent with Built-in WSS.', '使用「快速設定」透過 Built-in WSS 註冊此 Agent。', '使用“快速设置”通过 Built-in WSS 注册此 Agent。', 'クイック設定で Built-in WSS に Agent を登録してください。'],
  'Configuration pending restart': ['Configuration pending restart', '設定等待重新啟動套用', '设置等待重新启动应用', '設定は再起動後に適用'],
  'Current service': ['Current service', '目前服務', '当前服务', '現在のサービス'],
  'Recent activity': ['Recent activity', '最近活動', '最近活动', '最近のアクティビティ'],
  Result: ['Result', '結果', '结果', '結果'],
  Duration: ['Duration', '耗時', '耗时', '所要時間'],
  Success: ['Success', '成功', '成功', '成功'],
  Failed: ['Failed', '失敗', '失败', '失敗'],
  'No recent activity for this workspace.': ['No recent activity for this workspace.', '此工具區目前沒有近期活動。', '此工作区当前没有近期活动。', 'このワークスペースには最近のアクティビティがありません。'],
  'Browser management UI': ['Browser management UI', '瀏覽器管理介面', '浏览器管理界面', 'ブラウザー管理 UI'],
  'Rust-style navigation and setup flow': ['Rust-style navigation and setup flow', '比照 Rust UI 的導覽與設定流程', '参照 Rust UI 的导航与设置流程', 'Rust UI に合わせたナビゲーションと設定フロー'],
  'Management UI failed to load': ['Management UI failed to load', '管理介面載入失敗', '管理界面加载失败', '管理 UI の読み込みに失敗しました'],
  Retry: ['Retry', '重新嘗試', '重试', '再試行'],
  'Loading Agent status…': ['Loading Agent status…', '正在載入 Agent 狀態…', '正在加载 Agent 状态…', 'Agent の状態を読み込み中…'],
  'Light theme': ['Light theme', '淺色主題', '浅色主题', 'ライトテーマ'],
  'Dark theme': ['Dark theme', '深色主題', '深色主题', 'ダークテーマ'],
  'Some data failed to refresh': ['Some data failed to refresh', '部分資料更新失敗', '部分数据刷新失败', '一部のデータ更新に失敗しました'],
  'Saved configuration is applied after the Agent restarts.': ['Saved configuration is applied after the Agent restarts.', '已儲存的設定會在 Agent 重新啟動後套用。', '已保存的设置会在 Agent 重新启动后应用。', '保存した設定は Agent の再起動後に適用されます。'],
  'Last updated': ['Last updated', '最後更新', '最后更新', '最終更新'],
  'Local same-origin assets': ['Local same-origin assets', '本機同源資產', '本地同源资源', 'ローカル同一オリジン資産'],
  Configured: ['Configured', '已設定', '已设置', '設定済み'],
  'Not configured': ['Not configured', '未設定', '未设置', '未設定'],
  'OAuth client ID': ['OAuth client ID', 'OAuth 用戶端 ID', 'OAuth 客户端 ID', 'OAuth クライアント ID'],
  'Tool profile': ['Tool profile', '工具 Profile', '工具 Profile', 'ツールプロファイル'],
  'Permission mode': ['Permission mode', '權限模式', '权限模式', '権限モード'],
  Time: ['Time', '時間', '时间', '時刻'],
  'The wizard validates and securely saves every required value. Restart the Agent to register and start the tunnel.': ['The wizard validates and securely saves every required value. Restart the Agent to register and start the tunnel.', '引導會驗證並安全儲存所有必要值；重新啟動 Agent 後才會註冊裝置並啟動通道。', '向导会验证并安全保存所有必要值；重新启动 Agent 后才会注册设备并启动隧道。', '必要な値を検証して安全に保存します。Agent を再起動すると端末登録とトンネル起動が行われます。'],
  'Quick setup saved': ['Quick setup saved', '快速設定已儲存', '快速设置已保存', 'クイック設定を保存しました'],
  'Restart the Agent, then finish setup in ChatGPT': ['Restart the Agent, then finish setup in ChatGPT', '重新啟動 Agent，再到 ChatGPT 完成設定', '重新启动 Agent，再到 ChatGPT 完成设置', 'Agent を再起動してから ChatGPT で設定を完了してください'],
  'OAuth password must contain at least 12 characters.': ['OAuth password must contain at least 12 characters.', 'OAuth 密碼至少需要 12 個字元。', 'OAuth 密码至少需要 12 个字符。', 'OAuth パスワードは 12 文字以上にしてください。'],
  'Restart Agent': ['Restart Agent', '重新啟動 Agent', '重新启动 Agent', 'Agent を再起動'],
  'Restart the Agent now? Active tool calls and command sessions will be stopped.': ['Restart the Agent now? Active tool calls and command sessions will be stopped.', '現在重新啟動 Agent？進行中的工具呼叫與命令 Session 將會停止。', '现在重新启动 Agent？正在进行的工具调用和命令 Session 将会停止。', 'Agent を再起動しますか？実行中のツール呼び出しとコマンドセッションは停止します。'],
  'Restarting Agent…': ['Restarting Agent…', '正在重新啟動 Agent…', '正在重新启动 Agent…', 'Agent を再起動中…'],
  'The Agent is restarting. This page will reconnect automatically.': ['The Agent is restarting. This page will reconnect automatically.', 'Agent 正在重新啟動，此頁面會自動重新連線。', 'Agent 正在重新启动，此页面会自动重新连接。', 'Agent を再起動しています。このページは自動的に再接続します。'],
  'Restart request failed': ['Restart request failed', '重新啟動請求失敗', '重新启动请求失败', '再起動要求に失敗しました'],
  'Restart is available when launched with start-node-agent.bat.': ['Restart is available when launched with start-node-agent.bat.', '使用 start-node-agent.bat 啟動後即可從 Web UI 重新啟動。', '使用 start-node-agent.bat 启动后即可从 Web UI 重新启动。', 'start-node-agent.bat で起動すると Web UI から再起動できます。'],
  'Built-in WSS failed to start': ['Built-in WSS failed to start', 'Built-in WSS 啟動失敗', 'Built-in WSS 启动失败', 'Built-in WSS の起動に失敗しました'],
  'The local Agent is still running. Correct the Public MCP URL in Settings, save, and restart.': ['The local Agent is still running. Correct the Public MCP URL in Settings, save, and restart.', '本機 Agent 仍在運行。請到設定修正 Public MCP URL，儲存後重新啟動。', '本地 Agent 仍在运行。请到设置修正 Public MCP URL，保存后重新启动。', 'ローカル Agent は動作中です。設定で Public MCP URL を修正し、保存後に再起動してください。'],
  'Fix settings': ['Fix settings', '修正設定', '修正设置', '設定を修正'],
  Folders: ['Folders', '資料夾', '文件夹', 'フォルダー'],
  Overview: ['Overview', '概覽', '概览', '概要'],
  'Loading authorization password…': ['Loading authorization password…', '正在載入授權密碼…', '正在加载授权密码…', '認証パスワードを読み込み中…'],
  'Available anytime from this workspace overview.': ['Available anytime from this workspace overview.', '可隨時從此 Workspace 概覽查看。', '可随时从此 Workspace 概览查看。', 'このワークスペースの概要からいつでも確認できます。'],
  'Generating…': ['Generating…', '產生中…', '生成中…', '生成中…'],
  'Edit folders': ['Edit folders', '編輯資料夾', '编辑文件夹', 'フォルダーを編集'],
  'Each workspace has independent settings and folders.': ['Each workspace has independent settings and folders.', '每個 Workspace 都有獨立的設定與資料夾。', '每个 Workspace 都有独立的设置与文件夹。', '各ワークスペースには独立した設定とフォルダーがあります。'],
  'Add at least one folder to this workspace.': ['Add at least one folder to this workspace.', '請為此 Workspace 至少新增一個資料夾。', '请为此 Workspace 至少添加一个文件夹。', 'このワークスペースに少なくとも1つのフォルダーを追加してください。'],
  'Authorization password is unavailable. Refresh this page and try again.': ['Authorization password is unavailable. Refresh this page and try again.', '目前無法取得授權密碼，請重新整理頁面後再試。', '目前无法获取授权密码，请刷新页面后重试。', '認証パスワードを取得できません。ページを再読み込みして再試行してください。'],
  'This workspace has its own settings, authorization password, and folders.': ['This workspace has its own settings, authorization password, and folders.', '此 Workspace 擁有獨立的設定、授權密碼與資料夾。', '此 Workspace 拥有独立的设置、授权密码与文件夹。', 'このワークスペースには独立した設定、認証パスワード、フォルダーがあります。'],
  'After restart, copy the final Public MCP endpoint from the workspace overview and choose OAuth authentication.': ['After restart, copy the final Public MCP endpoint from the workspace overview and choose OAuth authentication.', '重新啟動後，從 Workspace 概覽複製正式 Public MCP 端點，並選擇 OAuth 驗證。', '重新启动后，从 Workspace 概览复制正式 Public MCP 端点，并选择 OAuth 验证。', '再起動後、ワークスペース概要から正式な Public MCP エンドポイントをコピーし、OAuth 認証を選択します。'],
  'Enter the OAuth Client ID and leave Client Secret empty.': ['Enter the OAuth Client ID and leave Client Secret empty.', '輸入 OAuth Client ID，Client Secret 保持空白。', '输入 OAuth Client ID，Client Secret 保持空白。', 'OAuth Client ID を入力し、Client Secret は空欄のままにします。'],
  'Select Next, click Connect, then enter the authorization password.': ['Select Next, click Connect, then enter the authorization password.', '選擇「下一步」、按下「連線」，再輸入授權密碼。', '选择“下一步”、点击“连接”，再输入授权密码。', '「次へ」を選び、「接続」をクリックして認証パスワードを入力します。'],
  'The final Public MCP endpoint is available on the workspace overview after enrollment completes.': ['The final Public MCP endpoint is available on the workspace overview after enrollment completes.', 'Enrollment 完成後，正式 Public MCP 端點會顯示在 Workspace 概覽。', 'Enrollment 完成后，正式 Public MCP 端点会显示在 Workspace 概览。', 'Enrollment 完了後、正式な Public MCP エンドポイントがワークスペース概要に表示されます。'],
  'The authorization password remains available from this workspace overview at any time.': ['The authorization password remains available from this workspace overview at any time.', '授權密碼之後仍可隨時從此 Workspace 概覽查看。', '授权密码之后仍可随时从此 Workspace 概览查看。', '認証パスワードは後からでもこのワークスペース概要でいつでも確認できます。'],
  History: ['History', '歷史', '历史', '履歴'],
  Telemetry: ['Telemetry', '遙測', '遥测', 'テレメトリ'],
  Logs: ['Logs', '操作記錄', '操作记录', '操作ログ'],
  'Operation log': ['Operation log', '操作記錄', '操作记录', '操作ログ'],
  'Browse persisted operation starts, completions, failures, and interrupted records without exposing commands or output.': ['Browse persisted operation starts, completions, failures, and interrupted records without exposing commands or output.', '查看持久化的操作開始、完成、失敗與中斷記錄，不會顯示命令或輸出內容。', '查看持久化的操作开始、完成、失败与中断记录，不会显示命令或输出内容。', 'コマンドや出力を公開せず、永続化された操作の開始、完了、失敗、中断記録を確認します。'],
  'Log folder': ['Log folder', '記錄資料夾', '记录文件夹', 'ログフォルダー'],
  Status: ['Status', '狀態', '状态', '状態'],
  'All statuses': ['All statuses', '所有狀態', '所有状态', 'すべての状態'],
  Completed: ['Completed', '已完成', '已完成', '完了'],
  Incomplete: ['Incomplete', '未完成', '未完成', '未完了'],
  'Tool filter': ['Tool filter', '工具篩選', '工具筛选', 'ツールフィルター'],
  'Failures and incomplete only': ['Failures and incomplete only', '僅顯示失敗與未完成', '仅显示失败与未完成', '失敗と未完了のみ'],
  Operations: ['Operations', '操作', '操作', '操作'],
  matched: ['matched', '符合', '匹配', '一致'],
  'Terminal success records': ['Terminal success records', '成功終態記錄', '成功终态记录', '成功した終端記録'],
  'Terminal failure records': ['Terminal failure records', '失敗終態記錄', '失败终态记录', '失敗した終端記録'],
  'Started without a terminal record': ['Started without a terminal record', '已開始但沒有終態記錄', '已开始但没有终态记录', '開始済みで終端記録なし'],
  'Loading operation logs…': ['Loading operation logs…', '正在載入操作記錄…', '正在加载操作记录…', '操作ログを読み込み中…'],
  'No operation logs yet': ['No operation logs yet', '目前沒有操作記錄', '目前没有操作记录', '操作ログはまだありません'],
  'Load older': ['Load older', '載入較舊記錄', '加载较旧记录', '古い記録を読み込む'],
  'Loading older…': ['Loading older…', '正在載入較舊記錄…', '正在加载较旧记录…', '古い記録を読み込み中…'],
  'Correlation ID': ['Correlation ID', '關聯 ID', '关联 ID', '相関 ID'],
  'Tracked task': ['Tracked task', '已追蹤任務', '已跟踪任务', '追跡対象タスク'],
  'Affected files': ['Affected files', '影響檔案', '影响文件', '影響ファイル'],
  Yes: ['Yes', '是', '是', 'はい'],
  No: ['No', '否', '否', 'いいえ'],
  Reason: ['Reason', '原因', '原因', '理由'],
  'Event timeline': ['Event timeline', '事件時間軸', '事件时间轴', 'イベントタイムライン'],
  Error: ['Error', '錯誤', '错误', 'エラー'],
  'Command result': ['Command result', '命令結果', '命令结果', 'コマンド結果'],
  Succeeded: ['Succeeded', '成功', '成功', '成功'],
  Verification: ['Verification', '驗證', '验证', '検証'],
  'Runtime result': ['Runtime result', '執行結果', '执行结果', 'ランタイム結果'],
  'Exit code': ['Exit code', '結束代碼', '退出代码', '終了コード'],
  Elapsed: ['Elapsed', '經過時間', '经过时间', '経過時間'],
  'First output': ['First output', '首次輸出', '首次输出', '初回出力'],
  Warnings: ['Warnings', '警告', '警告', '警告'],
  Timeouts: ['Timeouts', '逾時', '超时', 'タイムアウト'],
  Process: ['Process', '程序', '进程', 'プロセス'],
  Request: ['Request', '請求', '请求', 'リクエスト'],
  Retryable: ['Retryable', '可重試', '可重试', '再試行可能'],
  'Output size': ['Output size', '輸出大小', '输出大小', '出力サイズ'],
  'Wait time': ['Wait time', '等待時間', '等待时间', '待機時間'],
  'This operation started but has no terminal record. The Agent may have stopped or restarted before completion.': ['This operation started but has no terminal record. The Agent may have stopped or restarted before completion.', '此操作已開始但沒有終態記錄；Agent 可能在完成前停止或重新啟動。', '此操作已开始但没有终态记录；Agent 可能在完成前停止或重新启动。', 'この操作は開始済みですが終端記録がありません。完了前に Agent が停止または再起動した可能性があります。'],
  Health: ['Health', '健康檢查', '健康检查', 'ヘルス'],
  'Operation telemetry': ['Operation telemetry', '操作遙測', '操作遥测', '操作テレメトリ'],
  'Browse sanitized MCP tool calls, timings, outcomes, and errors for this workspace.': ['Browse sanitized MCP tool calls, timings, outcomes, and errors for this workspace.', '查看此 Workspace 經過清理的 MCP 工具呼叫、耗時、結果與錯誤。', '查看此 Workspace 经过清理的 MCP 工具调用、耗时、结果与错误。', 'このワークスペースのサニタイズ済み MCP ツール呼び出し、時間、結果、エラーを確認します。'],
  Scope: ['Scope', '範圍', '范围', '範囲'],
  'Current runtime': ['Current runtime', '目前執行個體', '当前运行实例', '現在のランタイム'],
  'Current version': ['Current version', '目前版本', '当前版本', '現在のバージョン'],
  'All retained': ['All retained', '所有保留資料', '所有保留数据', '保持データすべて'],
  Records: ['Records', '記錄', '记录', 'レコード'],
  'Sort by': ['Sort by', '排序依據', '排序依据', '並び順'],
  'Queue wait': ['Queue wait', '佇列等待', '队列等待', 'キュー待機'],
  'Request bytes': ['Request bytes', '請求 bytes', '请求 bytes', 'リクエスト bytes'],
  'Response bytes': ['Response bytes', '回應 bytes', '响应 bytes', 'レスポンス bytes'],
  Calls: ['Calls', '呼叫', '调用', '呼び出し'],
  Errors: ['Errors', '錯誤', '错误', 'エラー'],
  'Minimum duration': ['Minimum duration', '最低耗時（ms）', '最低耗时（ms）', '最小時間（ms）'],
  'Errors only': ['Errors only', '僅顯示錯誤', '仅显示错误', 'エラーのみ'],
  'Average duration': ['Average duration', '平均耗時', '平均耗时', '平均時間'],
  'P95 duration': ['P95 duration', 'P95 耗時', 'P95 耗时', 'P95 時間'],
  Tools: ['Tools', '工具', '工具', 'ツール'],
  'Telemetry aggregate': ['Telemetry aggregate', '遙測彙總', '遥测汇总', 'テレメトリ集計'],
  Tool: ['Tool', '工具', '工具', 'ツール'],
  'Loading telemetry…': ['Loading telemetry…', '正在載入遙測…', '正在加载遥测…', 'テレメトリを読み込み中…'],
  'No telemetry records yet': ['No telemetry records yet', '目前沒有遙測記錄', '目前没有遥测记录', 'テレメトリ記録はまだありません'],
  'Recent operations': ['Recent operations', '近期操作', '近期操作', '最近の操作'],
  'History sessions': ['History sessions', '歷史 Session', '历史 Session', '履歴セッション'],
  'Browse saved development sessions and checkpoint records for this workspace folder.': ['Browse saved development sessions and checkpoint records for this workspace folder.', '查看此 Workspace 資料夾已儲存的開發 Session 與 checkpoint 記錄。', '查看此 Workspace 文件夹已保存的开发 Session 与 checkpoint 记录。', 'このワークスペースフォルダーに保存された開発セッションとチェックポイントを確認します。'],
  'History folder': ['History folder', '歷史資料夾', '历史文件夹', '履歴フォルダー'],
  'History integrity warnings': ['History integrity warnings', '歷史完整性警告', '历史完整性警告', '履歴整合性の警告'],
  'Loading history…': ['Loading history…', '正在載入歷史記錄…', '正在加载历史记录…', '履歴を読み込み中…'],
  'No history sessions yet': ['No history sessions yet', '目前沒有歷史 Session', '目前没有历史 Session', '履歴セッションはまだありません'],
  Checkpoints: ['Checkpoints', 'Checkpoint', 'Checkpoint', 'チェックポイント'],
  'Session {number}': ['Session {number}', 'Session {number}', 'Session {number}', 'セッション {number}'],
  'No checkpoints recorded in this session.': ['No checkpoints recorded in this session.', '此 Session 沒有 checkpoint 記錄。', '此 Session 没有 checkpoint 记录。', 'このセッションにはチェックポイントがありません。'],
  'View raw Markdown record': ['View raw Markdown record', '查看原始 Markdown 記錄', '查看原始 Markdown 记录', 'Markdown 原文を表示'],
  Findings: ['Findings', '發現', '发现', '検出事項'],
  Decisions: ['Decisions', '決策', '决策', '決定事項'],
  'Files changed': ['Files changed', '變更檔案', '变更文件', '変更ファイル'],
  Tests: ['Tests', '測試', '测试', 'テスト'],
  'Runtime state': ['Runtime state', '執行狀態', '运行状态', 'ランタイム状態'],
  'Remaining issues': ['Remaining issues', '剩餘問題', '剩余问题', '残課題'],
  'Next actions': ['Next actions', '後續動作', '后续动作', '次のアクション'],
  'Health check': ['Health check', '健康檢查', '健康检查', 'ヘルスチェック'],
  'Validate the local MCP listener, OAuth metadata, and optional Built-in WSS runtime.': ['Validate the local MCP listener, OAuth metadata, and optional Built-in WSS runtime.', '驗證本機 MCP listener、OAuth metadata，以及選用的 Built-in WSS 執行狀態。', '验证本地 MCP listener、OAuth metadata，以及可选的 Built-in WSS 运行状态。', 'ローカル MCP リスナー、OAuth メタデータ、任意の Built-in WSS ランタイムを検証します。'],
  'Checking…': ['Checking…', '檢查中…', '检查中…', '確認中…'],
  'Run health check': ['Run health check', '執行健康檢查', '运行健康检查', 'ヘルスチェックを実行'],
  'All required checks passed.': ['All required checks passed.', '所有必要檢查皆通過。', '所有必要检查均已通过。', '必須チェックはすべて成功しました。'],
  'One or more required checks failed.': ['One or more required checks failed.', '至少一項必要檢查失敗。', '至少一项必要检查失败。', '1 つ以上の必須チェックが失敗しました。'],
  Passed: ['Passed', '通過', '通过', '成功'],
  Optional: ['Optional', '選用', '可选', '任意'],
  'No health check has been run.': ['No health check has been run.', '尚未執行健康檢查。', '尚未运行健康检查。', 'ヘルスチェックはまだ実行されていません。'],
  'Export diagnostics': ['Export diagnostics', '匯出診斷資料', '导出诊断数据', '診断情報をエクスポート'],
  'Exporting…': ['Exporting…', '匯出中…', '导出中…', 'エクスポート中…'],
  'Operational details': ['Operational details', '維運細節', '运维详情', '運用詳細'],
  'Dashboard contract': ['Dashboard contract', 'Dashboard 資料契約', 'Dashboard 数据契约', 'Dashboard データ契約'],
  'Pending permissions': ['Pending permissions', '待處理權限', '待处理权限', '保留中の権限'],
  None: ['None', '無', '无', 'なし'],
  'Persistent telemetry': ['Persistent telemetry', '持久化遙測', '持久化遥测', '永続テレメトリ'],
  'Tunnel workers': ['Tunnel workers', 'Tunnel workers', 'Tunnel workers', 'Tunnel workers'],
  'Tunnel requests': ['Tunnel requests', 'Tunnel 請求', 'Tunnel 请求', 'Tunnel リクエスト'],
  'Last request timeout': ['Last request timeout', '最近請求逾時', '最近请求超时', '直近のリクエストタイムアウト'],
  Never: ['Never', '從未', '从未', 'なし'],
  Menu: ['Menu', '選單', '菜单', 'メニュー'],
  Close: ['Close', '關閉', '关闭', '閉じる']
};

const RUST_MESSAGES = MESSAGES as unknown as Record<string, readonly string[]>;

type Translate = (key: string, values?: Record<string, string | number>) => string;

interface I18nValue {
  locale: Locale;
  setLocale(locale: Locale): void;
  options: ReadonlyArray<{ value: Locale; label: string }>;
  t: Translate;
}

const I18nContext = createContext<I18nValue | null>(null);

function initialLocale(): Locale {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (SUPPORTED_LOCALES.includes(saved as Locale)) return saved as Locale;
  } catch {
    // Browser storage is optional.
  }
  const browserLocale = navigator.language.toLowerCase();
  if (browserLocale.startsWith('zh-tw') || browserLocale.startsWith('zh-hk')) return 'zh-TW';
  if (browserLocale.startsWith('zh')) return 'zh-CN';
  if (browserLocale.startsWith('ja')) return 'ja';
  return 'en';
}

function interpolate(message: string, values: Record<string, string | number> = {}): string {
  return message.replace(/\{(\w+)\}/g, (match, name: string) => (
    Object.prototype.hasOwnProperty.call(values, name) ? String(values[name]) : match
  ));
}

export function I18nProvider({ children }: { children: ReactNode }) {
  const [locale, setLocale] = useState<Locale>(initialLocale);
  const localeIndex = SUPPORTED_LOCALES.indexOf(locale);

  useEffect(() => {
    document.documentElement.lang = locale;
    try {
      localStorage.setItem(STORAGE_KEY, locale);
    } catch {
      // Browser storage is optional.
    }
  }, [locale]);

  const value = useMemo<I18nValue>(() => ({
    locale,
    setLocale,
    options: LOCALE_OPTIONS,
    t: (key, values) => {
      const messages = RUST_MESSAGES[key] ?? UI_MESSAGES[key];
      return interpolate(messages?.[localeIndex] ?? messages?.[0] ?? key, values);
    }
  }), [locale, localeIndex]);

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function useI18n(): I18nValue {
  const value = useContext(I18nContext);
  if (!value) throw new Error('I18nProvider is missing.');
  return value;
}
