A plan is the latest step-by-step plan for the current task. It records the sequence of steps needed to finish the task and the current progress of each step.

Maintain a plan when the task is non-trivial, multi-step, or requires ongoing progress tracking, so current progress, the next step, and remaining work stay clear.

Use `update_plan` to maintain the plan. Each call must submit the complete current plan, not a patch for one step. Plan steps should be short, preferably 5 to 7 words, and must be concrete, actionable, and verifiable. While work remains, exactly one step must be `in_progress`; completed steps use `completed`, later steps use `pending`. When all steps are complete, clear the plan instead of retaining completed steps.
