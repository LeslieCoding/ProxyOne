import DnsOutlinedIcon from '@mui/icons-material/DnsOutlined'
import SettingsOutlinedIcon from '@mui/icons-material/SettingsOutlined'
import WifiOutlinedIcon from '@mui/icons-material/WifiOutlined'
import { type ComponentType, type ReactNode } from 'react'

import ProfilesSvg from '@/assets/image/itemicon/profiles.svg?react'
import ProxiesSvg from '@/assets/image/itemicon/proxies.svg?react'
import SettingsSvg from '@/assets/image/itemicon/settings.svg?react'

import { navigationItems } from './_navigation-meta'
import ProfilePage from './profiles'
import ProxyPage from './proxies'
import SettingPage from './settings'

type NavigationItem = {
  label: (typeof navigationItems)[keyof typeof navigationItems]['label']
  path: string
  icon: ReactNode[]
  Component: ComponentType
}

export const navItems: NavigationItem[] = [
  {
    ...navigationItems.proxies,
    icon: [<WifiOutlinedIcon key="mui" />, <ProxiesSvg key="svg" />],
    Component: ProxyPage,
  },
  {
    ...navigationItems.profiles,
    icon: [<DnsOutlinedIcon key="mui" />, <ProfilesSvg key="svg" />],
    Component: ProfilePage,
  },
  {
    ...navigationItems.settings,
    icon: [<SettingsOutlinedIcon key="mui" />, <SettingsSvg key="svg" />],
    Component: SettingPage,
  },
]
