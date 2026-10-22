import { Box, Button, ButtonGroup, Switch, Tooltip } from '@mui/material'
import { useLockFn } from 'ahooks'
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { closeAllConnections } from 'tauri-plugin-mihomo-api'

import { BasePage } from '@/components/base'
import { ProviderButton } from '@/components/proxy/provider-button'
import { ProxyGroups } from '@/components/proxy/proxy-groups'
import { useVerge } from '@/hooks/use-verge'
import {
  useAppRefreshers,
  useClashConfigData,
} from '@/providers/app-data-context'
import { patchClashMode } from '@/services/cmds'
import { showNotice } from '@/services/notice-service'

const MODES = ['rule', 'global', 'direct'] as const
type Mode = (typeof MODES)[number]
const MODE_SET = new Set<string>(MODES)
const isMode = (value: unknown): value is Mode =>
  typeof value === 'string' && MODE_SET.has(value)

const ProxyPage = () => {
  const { t } = useTranslation()

  const { clashConfig } = useClashConfigData()
  const { refreshClashConfig } = useAppRefreshers()
  const { verge, patchVerge } = useVerge()
  const [autoSelect, setAutoSelect] = useState(
    verge?.enable_auto_select ?? true,
  )

  const normalizedMode = clashConfig?.mode?.toLowerCase()
  const curMode = isMode(normalizedMode) ? normalizedMode : undefined

  // 自动选择开关（节点列表顶部）：开=刷新订阅+刷新节点后自动切最优节点；关=用户手选，保持不被顶回。
  const onChangeAutoSelect = useLockFn(async (next: boolean) => {
    setAutoSelect(next)
    try {
      await patchVerge({ enable_auto_select: next })
    } catch (error) {
      showNotice.error(error)
      setAutoSelect(!next)
    }
  })

  const onChangeMode = useLockFn(async (mode: Mode) => {
    // 断开连接
    if (mode !== curMode && verge?.auto_close_connection) {
      closeAllConnections()
    }
    try {
      // patchClashMode 在后端 PATCH 失败时会 reject，需提示用户而非静默失败
      await patchClashMode(mode)
      refreshClashConfig()
    } catch (error) {
      showNotice.error(error)
    }
  })

  useEffect(() => {
    if (normalizedMode && !isMode(normalizedMode)) {
      onChangeMode('rule')
    }
  }, [normalizedMode, onChangeMode])

  return (
    <BasePage
      full
      contentStyle={{ height: '100%' }}
      title={t('proxies.page.title.default')}
      header={
        <Box sx={{ display: 'flex', alignItems: 'center', gap: 2 }}>
          <ProviderButton />

          <Box sx={{ display: 'flex', alignItems: 'center', gap: 0.75 }}>
            <Switch
              checked={autoSelect}
              onChange={(e) => onChangeAutoSelect(e.target.checked)}
            />
            <Tooltip title={t('proxies.page.hints.proxyAutoSelect')} arrow>
              <Box sx={{ fontSize: '0.75rem', color: 'text.primary' }}>
                {t('proxies.page.labels.proxyAutoSelect')}
              </Box>
            </Tooltip>
          </Box>

          <ButtonGroup size="small">
            {MODES.map((mode) => (
              <Button
                key={mode}
                variant={mode === curMode ? 'contained' : 'outlined'}
                onClick={() => onChangeMode(mode)}
                sx={{ textTransform: 'capitalize' }}
              >
                {t(`proxies.page.modes.${mode}`)}
              </Button>
            ))}
          </ButtonGroup>
        </Box>
      }
    >
      <ProxyGroups
        mode={curMode ?? 'rule'}
        isChainMode={false}
        chainConfigData={null}
      />
    </BasePage>
  )
}

export default ProxyPage
