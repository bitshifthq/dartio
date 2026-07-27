import 'package:ptyx/ptyx.dart';
import 'package:test/test.dart';

void main() {
  test('typed exit statuses preserve their raw representation', () {
    expect(const PtyExited(259).rawCode, 259);
    expect(const PtySignaled(9).rawCode, -9);
    expect(const PtyExited(7), const PtyExited(7));
    expect(const PtySignaled(15), const PtySignaled(15));
  });
}
