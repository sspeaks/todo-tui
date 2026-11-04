// Native-only modules the screens import, replaced for Jest.
jest.mock("react-native-safe-area-context", () => jest.requireActual<{ default: unknown }>("react-native-safe-area-context/jest/mock").default);
jest.mock("@react-native-community/datetimepicker", () => ({ __esModule: true, default: () => null }));
jest.mock("expo-router", () => ({
  Stack: { Screen: () => null },
  router: { push: jest.fn(), back: jest.fn(), replace: jest.fn() },
}));
