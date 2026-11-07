package com.moss.laser

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.provider.Settings
import androidx.activity.enableEdgeToEdge
import androidx.annotation.Keep
import androidx.core.content.FileProvider
import java.io.File

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
  }
}

@Keep
object ApkInstaller {
  @Keep
  @JvmStatic
  fun installApk(context: Context, apkPath: String): String {
    return try {
      val apkFile = File(apkPath).canonicalFile
      val cacheRoot = File(context.cacheDir, "updates").canonicalFile
      if (!apkFile.isFile || !apkFile.path.startsWith(cacheRoot.path + File.separator)) {
        return "更新安装包路径无效"
      }
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O &&
        !context.packageManager.canRequestPackageInstalls()
      ) {
        val permIntent = Intent(
          Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
          Uri.parse("package:${context.packageName}")
        ).apply {
          addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        context.startActivity(permIntent)
        return "PERMISSION_REQUIRED:请在弹出的系统设置中允许 MOSS 安装应用更新，返回 MOSS 后再点一次按钮即可直接安装（无需重新下载）"
      }
      val uri = FileProvider.getUriForFile(
        context,
        "${context.packageName}.fileprovider",
        apkFile
      )
      val installIntent = Intent(Intent.ACTION_VIEW).apply {
        setDataAndType(uri, "application/vnd.android.package-archive")
        addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
      }
      context.startActivity(installIntent)
      ""
    } catch (e: Throwable) {
      "无法打开系统安装界面：${e.message ?: e.javaClass.simpleName}"
    }
  }
}
