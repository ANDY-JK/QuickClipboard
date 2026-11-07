import '@shared/styles/tabler-icons-subset.css';
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useSnapshot } from 'valtio';
import { open } from '@tauri-apps/plugin-dialog';
import { openAppDataFolder } from '@shared/api/pathActions';
import SettingsSection from '../components/SettingsSection';
import SettingItem from '../components/SettingItem';
import Toggle from '@shared/components/ui/Toggle';
import Button from '@shared/components/ui/Button';
import { toast } from '@shared/store/toastStore';
import { showConfirm } from '@shared/utils/dialog';
import { getDataDirectory } from '@shared/utils/dataDirectory';
import { installExtension, uninstallExtension } from '@shared/api/extensions';
import {
  disposeExtension,
  extensionsStore,
  initExtensions,
  preflightExtensions,
  reloadExtensions,
  syncExtensionsEnabled,
} from '@/extensions/host/registry';

// 后端的错误是给用户看的原文（如「安装包条目包含非法路径: ../evil.js」），
// 这里只做字符串化，不要走 formatUserMessage 把它归一成通用文案。
function errorText(error) {
  if (typeof error === 'string') return error;
  if (error && typeof error.message === 'string') return error.message;
  return String(error ?? '');
}

function ExtensionsSection({ settings, onSettingChange }) {
  const { t } = useTranslation();
  const snap = useSnapshot(extensionsStore);
  const [extensionsDir, setExtensionsDir] = useState('');
  const [openError, setOpenError] = useState('');
  const [reloading, setReloading] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [uninstallingId, setUninstallingId] = useState('');

  useEffect(() => {
    // 设置窗口的注册表默认 enabled=true；必须先用已保存的开关状态校准，
    // 否则「停用」状态下设置页仍会预检并创建 Worker。
    syncExtensionsEnabled(settings.extensionsEnabled !== false);
    initExtensions()
      // 设置页主动做一次加载预检，让入口缺失/导出不合规在列表里立即可见。
      .then(() => preflightExtensions())
      .catch((error) => console.warn('扩展预检失败:', error));
  }, [settings.extensionsEnabled]);

  useEffect(() => {
    getDataDirectory()
      .then((dir) => setExtensionsDir(`${dir.replace(/[\\/]+$/, '')}/extensions`))
      .catch(() => setExtensionsDir(''));
  }, []);

  const handleOpenDirectory = async () => {
    if (!extensionsDir) {
      return;
    }
    try {
      await openAppDataFolder('extensions');
      setOpenError('');
    } catch (error) {
      setOpenError(String((error && error.message) || error));
    }
  };

  // 安装/卸载/重载后的统一收口：重新扫描磁盘 -> 同步启用态 -> 后台预检加载。
  // 预检不阻塞按钮状态，失败时在列表里逐项展示。
  const refreshExtensions = async (reason) => {
    await reloadExtensions({ reason });
    syncExtensionsEnabled(settings.extensionsEnabled !== false);
    preflightExtensions().catch((error) => console.warn('扩展预检失败:', error));
  };

  const handleReload = async () => {
    setReloading(true);
    try {
      await refreshExtensions('manual-reload');
      toast.success(t('settings.extensions.reloaded'));
    } catch (error) {
      toast.error(t('settings.extensions.reloadFailed'));
      console.warn('重新加载扩展失败:', error);
    } finally {
      setReloading(false);
    }
  };

  // 安装的统一收口：冲突时先问用户，确认后带 overwrite 再调一次后端。
  const installFrom = async (source) => {
    setInstalling(true);
    try {
      const result = await installExtension(source, false);
      if (result.status === 'conflict') {
        const confirmed = await showConfirm(
          t('settings.extensions.overwriteConfirm', { id: result.id }),
          t('settings.extensions.install')
        );
        if (!confirmed) {
          return;
        }
        const overwritten = await installExtension(source, true);
        if (overwritten.status === 'conflict') {
          toast.error(t('settings.extensions.installFailed'), { duration: 6000 });
          return;
        }
        await refreshExtensions('install');
        toast.success(
          t('settings.extensions.installedOverwrite', { name: overwritten.name || overwritten.id })
        );
        return;
      }

      await refreshExtensions('install');
      toast.success(t('settings.extensions.installed', { name: result.name || result.id }));
    } catch (error) {
      // 安装失败也重新扫描一次，确保界面与磁盘真实状态一致（后端已负责清理临时目录）。
      await refreshExtensions('install-failed').catch(() => {});
      // 失败原因原样展示，便于用户判断是包坏了还是路径不对。
      toast.error(`${t('settings.extensions.installFailed')}: ${errorText(error)}`, {
        duration: 8000,
      });
    } finally {
      setInstalling(false);
    }
  };

  const handleInstallFromZip = async () => {
    try {
      const picked = await open({
        multiple: false,
        directory: false,
        filters: [{ name: t('settings.extensions.zipFilter'), extensions: ['zip'] }],
      });
      if (typeof picked === 'string' && picked) {
        await installFrom(picked);
      }
    } catch (error) {
      toast.error(`${t('settings.extensions.installFailed')}: ${errorText(error)}`, {
        duration: 8000,
      });
    }
  };

  const handleInstallFromFolder = async () => {
    try {
      const picked = await open({ multiple: false, directory: true });
      if (typeof picked === 'string' && picked) {
        await installFrom(picked);
      }
    } catch (error) {
      toast.error(`${t('settings.extensions.installFailed')}: ${errorText(error)}`, {
        duration: 8000,
      });
    }
  };

  const handleUninstall = async (ext) => {
    const name = ext.name || ext.id;
    const confirmed = await showConfirm(
      t('settings.extensions.uninstallConfirm', { name }),
      t('settings.extensions.uninstall', { name })
    );
    if (!confirmed) {
      return;
    }

    setUninstallingId(ext.id);
    try {
      // 先释放 Worker 再删目录，避免 Windows 上文件被占用导致删除失败。
      disposeExtension(ext.id);
      await uninstallExtension(ext.id);
      await refreshExtensions('uninstall');
      toast.success(t('settings.extensions.uninstalled', { name }));
    } catch (error) {
      // 删除失败时把磁盘状态重新同步回来，避免界面显示一个其实还在的扩展。
      await refreshExtensions('uninstall-failed').catch(() => {});
      toast.error(`${t('settings.extensions.uninstallFailed')}: ${errorText(error)}`, {
        duration: 8000,
      });
    } finally {
      setUninstallingId('');
    }
  };

  // 清单级加载失败（入口缺失/导出不合规/Worker 崩溃）的聚合提示，
  // 避免只有逐条小字错误、整体看起来像「一切正常」。
  const loadFailedCount = snap.extensions.reduce(
    (count, ext) => (ext.valid && snap.errors[ext.id] ? count + 1 : count),
    0
  );

  return (
    <>
      <SettingsSection
        title={t('settings.extensions.title')}
        description={t('settings.extensions.description')}
      >
        <SettingItem
          label={t('settings.extensions.enable')}
          description={t('settings.extensions.enableDesc')}
        >
          <Toggle
            checked={settings.extensionsEnabled !== false}
            onChange={(checked) => {
              syncExtensionsEnabled(checked);
              onSettingChange('extensionsEnabled', checked);
              if (checked) {
                // 启用后立即预检，加载失败无需等主窗口使用就能在列表里看到。
                preflightExtensions().catch((error) => console.warn('扩展预检失败:', error));
              }
            }}
          />
        </SettingItem>

        <SettingItem
          label={t('settings.extensions.directory')}
          description={extensionsDir || t('common.loading')}
        >
          <div className="flex flex-col items-end gap-2">
            <Button
              variant="secondary"
              icon={<i className="ti ti-folder-open"></i>}
              onClick={handleOpenDirectory}
              disabled={!extensionsDir}
            >
              {t('settings.extensions.openDirectory')}
            </Button>
            {openError && (
              <span className="max-w-72 text-right text-xs leading-5 text-red-500">
                {t('settings.extensions.directoryUnavailable')}
              </span>
            )}
          </div>
        </SettingItem>

        <SettingItem
          label={t('settings.extensions.install')}
          description={t('settings.extensions.installDesc')}
        >
          <div className="flex items-center gap-2">
            <Button
              variant="secondary"
              icon={<i className="ti ti-file-zip"></i>}
              onClick={handleInstallFromZip}
              disabled={installing}
            >
              {installing ? t('settings.extensions.installing') : t('settings.extensions.installFromZip')}
            </Button>
            <Button
              variant="secondary"
              icon={<i className="ti ti-folder-plus"></i>}
              onClick={handleInstallFromFolder}
              disabled={installing}
            >
              {t('settings.extensions.installFromFolder')}
            </Button>
          </div>
        </SettingItem>

        <div className="py-3.5 border-b border-qc-border">
          <div className="mb-3 flex items-center justify-between">
            <label className="block text-sm font-semibold leading-5 text-qc-fg">
              {t('settings.extensions.list')}
            </label>
            <Button
              variant="secondary"
              icon={<i className="ti ti-refresh"></i>}
              onClick={handleReload}
              disabled={reloading}
            >
              {t('settings.extensions.reload')}
            </Button>
          </div>

          {settings.extensionsEnabled !== false && loadFailedCount > 0 && (
            <p className="mb-3 text-xs leading-5 text-red-500">
              {t('settings.extensions.loadFailedHint', { count: loadFailedCount })}
            </p>
          )}

          {snap.extensions.length === 0 ? (
            <p className="text-xs leading-5 text-qc-fg-subtle">{t('settings.extensions.empty')}</p>
          ) : (
            <div className="flex flex-col gap-2">
              {snap.extensions.map((ext) => {
                const runtimeError = snap.errors[ext.id];
                const isValid = ext.valid && !runtimeError;
                return (
                  <div
                    key={ext.id}
                    className="flex items-start justify-between gap-4 rounded-lg border border-qc-border px-3 py-2.5"
                  >
                    <div className="min-w-0">
                      <div className="flex items-center gap-2">
                        <span className="truncate text-sm font-semibold text-qc-fg">
                          {ext.name || ext.id}
                        </span>
                        {ext.version && (
                          <span className="flex-shrink-0 text-xs text-qc-fg-subtle">
                            v{ext.version}
                          </span>
                        )}
                      </div>
                      {ext.description && (
                        <p className="mt-0.5 truncate text-xs leading-5 text-qc-fg-subtle">
                          {ext.description}
                        </p>
                      )}
                      {!isValid && (runtimeError || ext.error) && (
                        <p className="mt-1 text-xs leading-5 text-red-500">
                          {t('settings.extensions.error')}: {runtimeError || ext.error}
                        </p>
                      )}
                    </div>
                    <div className="flex flex-shrink-0 flex-col items-end gap-2">
                      <span
                        className={`rounded-full px-2 py-0.5 text-[10px] leading-4 ${
                          isValid
                            ? 'bg-qc-active text-qc-fg'
                            : 'bg-red-500/10 text-red-500'
                        }`}
                      >
                        {isValid
                          ? t('settings.extensions.statusValid')
                          : t('settings.extensions.statusInvalid')}
                      </span>
                      <Button
                        variant="secondary"
                        size="sm"
                        icon={<i className="ti ti-trash"></i>}
                        loading={uninstallingId === ext.id}
                        disabled={uninstallingId === ext.id}
                        aria-label={t('settings.extensions.uninstall', { name: ext.name || ext.id })}
                        title={t('settings.extensions.uninstall', { name: ext.name || ext.id })}
                        onClick={() => handleUninstall(ext)}
                      >
                        {uninstallingId === ext.id
                          ? t('settings.extensions.uninstalling')
                          : t('common.delete')}
                      </Button>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>

        <div className="pt-3 text-xs leading-5 text-qc-fg-subtle">
          {t('settings.extensions.entryDoc')}
        </div>
      </SettingsSection>
    </>
  );
}

export default ExtensionsSection;
