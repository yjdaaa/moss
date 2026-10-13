; MOSS 安装器钩子（技术书 §9.6；docs/developer/packaging-install.md §3.4–3.5）
; 由 tauri.conf.json 的 bundle.windows.nsis.installerHooks 引入，插入 Tauri 自带 NSIS 模板。
;
; 1. 安装 / 卸载前：MOSS 正在运行时只提示用户自己退出，然后取消。
;    这些钩子在 Tauri 自带的 CheckIfAppIsRunning 之前执行；自带检查会询问后强行结束进程，
;    而强杀会跳过 ShutdownCoordinator 的急停顺序，所以由这里先拦下来，自带的强杀不会被触发。
; 2. 卸载时用户勾选“删除应用程序数据”才删除 MOSS 实际使用的 %APPDATA%\moss；
;    Tauri 自带选项只删 %APPDATA%\com.moss.laser，覆盖不到。默认不勾选。
;    不触碰用户自备的 xiaozhi-server / mcp-endpoint-server 目录与导出文件。

!macro MOSS_ABORT_IF_RUNNING EXE_NAME
  nsis_tauri_utils::FindProcess "${EXE_NAME}"
  Pop $R0
  ${If} $R0 = 0
    IfSilent +2
    MessageBox MB_ICONEXCLAMATION|MB_OK "检测到 MOSS 正在运行（${EXE_NAME}）。$\r$\n$\r$\n为保证激光任务按安全顺序停止，安装程序不会强行结束它。$\r$\n请先在托盘图标上右键选择“退出程序”，然后重新运行本程序。"
    Abort "MOSS 正在运行，已取消"
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro MOSS_ABORT_IF_RUNNING "moss-desktop.exe"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro MOSS_ABORT_IF_RUNNING "moss-desktop.exe"
  !insertmacro MOSS_ABORT_IF_RUNNING "moss-host.exe"
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    SetShellVarContext current
    ; 只删除固定路径 %APPDATA%\moss；APPDATA 为空时跳过，绝不删除其他目录
    StrCmp $APPDATA "" moss_skip_data_delete
    IfFileExists "$APPDATA\moss\*.*" 0 moss_skip_data_delete
    RmDir /r "$APPDATA\moss"
    moss_skip_data_delete:
  ${EndIf}
!macroend
