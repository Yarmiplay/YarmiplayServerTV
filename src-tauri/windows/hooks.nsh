; Installer hooks for the NSIS setup (Tauri calls these for a normal install and for /S).
; Settings live in %APPDATA%\Yarmiplay and %LOCALAPPDATA%\Yarmiplay. This file must not delete them.

!define YARMI_MSI_UPGRADE "{6F3E2B1C-8D4A-4F7E-9B2C-5A1D3E7F9C42}"

Var YarmiBanner
Var YarmiBitmap

!macro NSIS_HOOK_PREINSTALL
  StrCpy $YarmiBanner 0
  StrCpy $YarmiBitmap 0
  ; Silent installs (the updater passes /S) have no wizard page. Show a small window instead.
  IfSilent 0 yarmi_after_banner
  Call YarmiShowBanner
yarmi_after_banner:
  Call YarmiRemoveMsi
!macroend

!macro NSIS_HOOK_POSTINSTALL
  Call YarmiHideBanner
!macroend

; One UAC prompt: msiexec elevates itself to remove a per-machine MSI from 1.9.x.
; The product code comes back in $1. Nothing under AppData is removed.
Function YarmiRemoveMsi
  StrCpy $R0 0
yarmi_msi_loop:
  System::Call 'msi::MsiEnumRelatedProductsW(w "${YARMI_MSI_UPGRADE}", i 0, i $R0, w .r1) i.r2'
  IntCmp $2 0 0 yarmi_msi_done yarmi_msi_done
  ExecWait 'msiexec.exe /x $1 /qn /norestart'
  IntOp $R0 $R0 + 1
  IntCmp $R0 8 yarmi_msi_done yarmi_msi_loop yarmi_msi_done
yarmi_msi_done:
FunctionEnd

; Dark window in the control panel's colours: background #0E1116, text #E8ECF2, bar #3DA5F4.
; Drawn into a bitmap. A one-shot DrawText on the window itself is erased on the next paint.
Function YarmiShowBanner
  System::Call 'user32::GetDC(i 0) i.r6'
  IntCmp $6 0 yarmi_banner_done
  System::Call 'gdi32::CreateCompatibleDC(i r6) i.r7'
  System::Call 'gdi32::CreateCompatibleBitmap(i r6, i 440, i 150) i.s'
  Pop $YarmiBitmap
  System::Call 'gdi32::SelectObject(i r7, i $YarmiBitmap) i.r9'
  System::Call 'user32::ReleaseDC(i 0, i r6)'
  ; COLORREF is 0x00BBGGRR. #0E1116 -> 0x0016110E, #E8ECF2 -> 0x00F2ECE8, #3DA5F4 -> 0x00F4A53D.
  System::Call '*(i 0, i 0, i 440, i 150) p.r5'
  System::Call 'gdi32::CreateSolidBrush(i 0x0016110E) i.r3'
  System::Call 'user32::FillRect(i r7, p r5, i r3)'
  System::Call 'gdi32::DeleteObject(i r3)'
  System::Call 'gdi32::SetBkMode(i r7, i 1)'
  System::Call 'gdi32::SetTextColor(i r7, i 0x00F2ECE8)'
  System::Call 'user32::DrawText(i r7, t "Installing YarmiplayServerTV...", i -1, p r5, i 0x25)'
  System::Call 'gdi32::CreateSolidBrush(i 0x00F4A53D) i.r3'
  System::Call '*(i 24, i 118, i 416, i 126) p.r4'
  System::Call 'user32::FillRect(i r7, p r4, i r3)'
  System::Call 'gdi32::DeleteObject(i r3)'
  System::Call 'gdi32::SelectObject(i r7, i r9)'
  System::Call 'gdi32::DeleteDC(i r7)'
  System::Free $r5
  System::Free $r4
  ; WS_EX_TOPMOST|WS_EX_TOOLWINDOW, and WS_POPUP|WS_VISIBLE|WS_BORDER|SS_BITMAP.
  System::Call 'user32::CreateWindowEx(i 0x88, t "Static", t "", i 0x9080000E, i 0, i 0, i 440, i 150, i 0, i 0, i 0, i 0) i.s'
  Pop $YarmiBanner
  IntCmp $YarmiBanner 0 yarmi_banner_fail
  System::Call 'user32::GetSystemMetrics(i 0) i.r3'
  System::Call 'user32::GetSystemMetrics(i 1) i.r4'
  IntOp $3 $3 - 440
  IntOp $3 $3 / 2
  IntOp $4 $4 - 150
  IntOp $4 $4 / 2
  System::Call 'user32::MoveWindow(i $YarmiBanner, i r3, i r4, i 440, i 150, i 1)'
  ; STM_SETIMAGE = 0x172, IMAGE_BITMAP = 0. The control repaints this bitmap.
  System::Call 'user32::SendMessage(i $YarmiBanner, i 0x172, i 0, i $YarmiBitmap)'
  System::Call 'user32::ShowWindow(i $YarmiBanner, i 5)'
  System::Call 'user32::SetForegroundWindow(i $YarmiBanner)'
  Goto yarmi_banner_done
yarmi_banner_fail:
  System::Call 'gdi32::DeleteObject(i $YarmiBitmap)'
  StrCpy $YarmiBitmap 0
yarmi_banner_done:
FunctionEnd

Function YarmiHideBanner
  IntCmp $YarmiBanner 0 yarmi_hide_bmp
  System::Call 'user32::DestroyWindow(i $YarmiBanner)'
  StrCpy $YarmiBanner 0
yarmi_hide_bmp:
  IntCmp $YarmiBitmap 0 yarmi_hide_done
  System::Call 'gdi32::DeleteObject(i $YarmiBitmap)'
  StrCpy $YarmiBitmap 0
yarmi_hide_done:
FunctionEnd
