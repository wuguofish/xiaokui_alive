!define XIAOKUI_DISCORD_BOT_SOURCE "${__FILEDIR__}\..\resources\discord-bot"
!define XIAOKUI_DISCORD_BOT_TARGET "$INSTDIR\resources\discord-bot"
!define XIAOKUI_DISCORD_BOT_LEGACY_TARGET "$INSTDIR\discord-bot"

!macro NSIS_HOOK_PREINSTALL
  IfFileExists "${XIAOKUI_DISCORD_BOT_SOURCE}\xiaokui_bot.exe" 0 xiaokui_bot_hook_done

  Delete "${XIAOKUI_DISCORD_BOT_LEGACY_TARGET}\xiaokui_bot.exe"
  Delete "${XIAOKUI_DISCORD_BOT_LEGACY_TARGET}\config_xiaokui.json"
  Delete "${XIAOKUI_DISCORD_BOT_LEGACY_TARGET}\.env.xiaokui.example"
  RMDir /r "${XIAOKUI_DISCORD_BOT_LEGACY_TARGET}\_internal"
  RMDir "${XIAOKUI_DISCORD_BOT_LEGACY_TARGET}"

  CreateDirectory "${XIAOKUI_DISCORD_BOT_TARGET}"
  SetOutPath "${XIAOKUI_DISCORD_BOT_TARGET}"
  File "${XIAOKUI_DISCORD_BOT_SOURCE}\xiaokui_bot.exe"
  File "${XIAOKUI_DISCORD_BOT_SOURCE}\config_xiaokui.json"
  File "${XIAOKUI_DISCORD_BOT_SOURCE}\.env.xiaokui.example"

  CreateDirectory "${XIAOKUI_DISCORD_BOT_TARGET}\_internal"
  SetOutPath "${XIAOKUI_DISCORD_BOT_TARGET}\_internal"
  File /r "${XIAOKUI_DISCORD_BOT_SOURCE}\_internal\*"

  SetOutPath "$INSTDIR"

  xiaokui_bot_hook_done:
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  Delete "${XIAOKUI_DISCORD_BOT_TARGET}\xiaokui_bot.exe"
  Delete "${XIAOKUI_DISCORD_BOT_TARGET}\config_xiaokui.json"
  Delete "${XIAOKUI_DISCORD_BOT_TARGET}\.env.xiaokui.example"
  RMDir /r "${XIAOKUI_DISCORD_BOT_TARGET}\_internal"
  RMDir "${XIAOKUI_DISCORD_BOT_TARGET}"
  Delete "${XIAOKUI_DISCORD_BOT_LEGACY_TARGET}\xiaokui_bot.exe"
  Delete "${XIAOKUI_DISCORD_BOT_LEGACY_TARGET}\config_xiaokui.json"
  Delete "${XIAOKUI_DISCORD_BOT_LEGACY_TARGET}\.env.xiaokui.example"
  RMDir /r "${XIAOKUI_DISCORD_BOT_LEGACY_TARGET}\_internal"
  RMDir "${XIAOKUI_DISCORD_BOT_LEGACY_TARGET}"
  RMDir "$INSTDIR\resources"
!macroend
