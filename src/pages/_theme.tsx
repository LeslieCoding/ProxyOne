import getSystem from '@/utils/get-system'
const OS = getSystem()

// default (light) theme setting — kept light so explicit "light" mode works
export const defaultTheme = {
  primary_color: '#FF4D4D',
  secondary_color: '#FFB74D',
  primary_text: '#1D1D1F',
  secondary_text: '#6E6E73',
  info_color: '#29B6F6',
  error_color: '#FF5252',
  warning_color: '#FFB74D',
  success_color: '#69F0AE',
  background_color: '#FAFAF9',
  font_family: `-apple-system, BlinkMacSystemFont,"Microsoft YaHei UI", "Microsoft YaHei", Roboto, "Helvetica Neue", Arial, sans-serif, "Apple Color Emoji"${
    OS === 'windows' ? ', twemoji mozilla' : ''
  }`,
}

// "dark" mode — medium dark background (#1A1A1A, not near-black), near-white readable text, bright red accent
export const defaultDarkTheme = {
  ...defaultTheme,
  primary_text: '#FFFFFF',
  secondary_text: '#B0B0B3',
  background_color: '#1A1A1A',
}
