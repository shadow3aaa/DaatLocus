package io.shadow3aaa.daatlocus.fixture.phoneuse;

import android.app.Activity;
import android.os.Bundle;
import android.graphics.Color;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.TextView;

/** Framework-only disposable GUI; each task has independent mutable state. */
public final class ProbeActivity extends Activity {
    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        getWindow().setDecorFitsSystemWindows(false);
        LinearLayout content = new LinearLayout(this);
        content.setOrientation(LinearLayout.VERTICAL);
        content.setBackgroundColor(Color.WHITE);
        TextView title = new TextView(this);
        title.setText("PhoneUse native fixture: task " + getTaskId() + ", display " + getDisplay().getDisplayId());
        content.addView(title, new LinearLayout.LayoutParams(-1, dp(64)));
        EditText text = new EditText(this);
        text.setSingleLine(true);
        text.setHint("Unicode receiver");
        text.setId(1234);
        content.addView(text, new LinearLayout.LayoutParams(-1, dp(64)));
        setContentView(content);
    }
    private int dp(int value) { return Math.round(value * getResources().getDisplayMetrics().density); }
}
