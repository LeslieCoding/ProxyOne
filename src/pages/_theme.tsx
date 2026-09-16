import getSystem from '@/utils/get-system'
const OS = getSystem()

// default theme setting
export const defaultTheme = {
  primary_color: '#E60012',
  secondary_color: '#E6A23C',
  primary_text: '#1D1D1F',
  secondary_text: '#6E6E73',
  info_color: '#0A84FF',
  error_color: '#D32F2F',
  warning_color: '#E6A23C',
  success_color: '#1FA84F',
  background_color: '#FAFAF9',
  font_family: `-apple-system, BlinkMacSystemFont,"Microsoft YaHei UI", "Microsoft YaHei", Roboto, "Helvetica Neue", Arial, sans-serif, "Apple Color Emoji"${
    OS === 'windows' ? ', twemoji mozilla' : ''
  }`,
}

// "dark" mode — kept as a light red/white variant so nothing renders near-black
export const defaultDarkTheme = {
  ...defaultTheme,
  background_color: '#F5F4F2',
}
