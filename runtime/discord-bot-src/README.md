# discord-bot-src

這個目錄是 `xiaokui_alive` 要打包進安裝檔的 Discord Bot 發行來源。

原則：

- 只放可分發需要的來源與範例設定
- 不放實際 token 或本機 `.env.xiaokui`
- Bot 的 build 產物不進這個目錄

本機設定（兩個範本都要複製後填入自己的值）：

- 複製 `.env.xiaokui.example` 為 `.env.xiaokui`，填入 `DISCORD_TOKEN_XIAOKUI` 等設定
- 複製 `config_xiaokui.example.json` 為 `config_xiaokui.json`，填入你的 Discord user id（`allowFrom`）與頻道 id（`channels`）

建置方式：

```powershell
powershell -ExecutionPolicy Bypass -File scripts/build_discord_bot.ps1
```

輸出位置：

```text
src-tauri/resources/discord-bot/
```
