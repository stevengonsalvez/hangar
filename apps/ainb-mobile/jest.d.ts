// expo-router/testing-library registers these matchers at runtime but ships
// no typings for them.
declare namespace jest {
  interface Matchers<R> {
    toHavePathname(pathname: string): R;
    toHaveSegments(segments: string[]): R;
    toHaveSearchParams(params: Record<string, string>): R;
  }
}
