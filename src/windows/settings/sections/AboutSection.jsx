import '@shared/styles/tabler-icons-subset.css';
import { useTranslation } from 'react-i18next';
import { useState, useEffect } from 'react';
import { openUrl } from '@tauri-apps/plugin-opener';
import { getAppVersion } from '@shared/services/settingsService';
import { copyTextToClipboard } from '@shared/api';
import { toast } from '@shared/store/toastStore';
import SettingsSection from '../components/SettingsSection';
import Button from '@shared/components/ui/Button';
import logoIcon from '@/assets/icon1024.png';
import appLinks from '@shared/config/appLinks.json';
import { MAINTAINER_NAME } from '@shared/config/maintainer';

function AboutSection() {
  const {
    t
  } = useTranslation();
  const [version, setVersion] = useState('1.0.0');
  
  useEffect(() => {
    const fetchVersion = async () => {
      try {
        const versionInfo = await getAppVersion();
        setVersion(versionInfo.version || '1.0.0');
      } catch (error) {
        console.error('获取版本信息失败:', error);
        setVersion('1.0.0');
      }
    };
    
    fetchVersion();
  }, []);
  
  const handleCopyVersion = async () => {
    try {
      await copyTextToClipboard(`${t('settings.about.appName')} v${version}`);
      toast.success(t('settings.about.versionCopied'));
    } catch (error) {
      console.error('复制版本号失败:', error);
      toast.error(t('common.operationFailed'));
    }
  };
  const handleOpenGitHub = async () => {
    try {
      await openUrl(appLinks.github);
    } catch (error) {
      console.error('打开GitHub链接失败:', error);
    }
  };
  return (
    <SettingsSection title={t('settings.about.title')} description={t('settings.about.description')}>
      <div className="space-y-6">
        <div className="text-center">
          <div className="inline-flex items-center justify-center w-16 h-16 bg-blue-100 rounded-full mb-3 overflow-hidden">
            <img src={logoIcon} alt="QuickClipboard Logo" className="w-12 h-12 object-contain" />
          </div>
          <h3 className="text-xl font-bold text-qc-fg mb-1">
            {t('settings.about.appName')}
          </h3>
          <div
            className="inline-flex items-center gap-1 cursor-pointer text-sm text-qc-fg-muted mb-3 hover:text-qc-fg transition-colors"
            onClick={handleCopyVersion}
            title={t('settings.about.clickToCopyVersion')}
          >
            <span>{t('settings.about.version')} {version}</span>
            <i className="ti ti-copy text-xs opacity-60"></i>
          </div>
          <p className="text-sm text-qc-fg-muted max-w-md mx-auto">
            {t('settings.about.descriptionText')}
          </p>
        </div>

        <div className="flex flex-wrap gap-2 justify-center">
          <Button variant="secondary" icon={<i className="ti ti-brand-github"></i>} onClick={handleOpenGitHub}>
            GitHub
          </Button>
        </div>

        <div className="rounded-lg border border-qc-border bg-qc-panel px-4 py-3">
          <p className="text-xs text-qc-fg-muted leading-relaxed text-center">
            {t('settings.about.creditsText', { maintainer: MAINTAINER_NAME })}
          </p>
        </div>
      </div>
    </SettingsSection>
  );
}
export default AboutSection;
