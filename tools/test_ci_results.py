import copy
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("ci_results", Path(__file__).with_name("check-ci-results.py"))
ci_results = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ci_results)


class CiResultsTests(unittest.TestCase):
    def needs(self, *selected):
        jobs = ("frontend", "rust", "native", "ci")
        return {
            "changes": {"result": "success", "outputs": {job: str(job in selected).lower() for job in jobs}},
            **{job: {"result": "success" if job in selected else "skipped"} for job in jobs},
        }

    def test_docs_can_skip_application_jobs(self):
        self.assertEqual(ci_results.result_errors(self.needs()), [])

    def test_selected_jobs_must_succeed(self):
        for selected in [("frontend",), ("rust", "native"), ("frontend", "rust", "native", "ci")]:
            self.assertEqual(ci_results.result_errors(self.needs(*selected)), [])
            for job in selected:
                for result in ("failure", "cancelled", "skipped", None):
                    needs = self.needs(*selected)
                    needs[job]["result"] = result
                    with self.subTest(job=job, result=result):
                        self.assertTrue(ci_results.result_errors(needs))

    def test_failed_path_detection_cannot_pass_with_skipped_dependents(self):
        for result in ("failure", "cancelled", "skipped", None):
            needs = self.needs()
            needs["changes"]["result"] = result
            self.assertTrue(ci_results.result_errors(needs))

    def test_missing_or_invalid_outputs_cannot_silently_skip_checks(self):
        valid = self.needs()
        for job in ("frontend", "rust", "native", "ci"):
            for value in (None, "", "TRUE", True):
                needs = copy.deepcopy(valid)
                needs["changes"]["outputs"][job] = value
                self.assertTrue(ci_results.result_errors(needs))

    def test_unselected_failures_and_missing_jobs_are_rejected(self):
        for result in ("failure", "cancelled", None):
            needs = self.needs()
            needs["rust"]["result"] = result
            self.assertTrue(ci_results.result_errors(needs))
        needs = self.needs()
        del needs["native"]
        self.assertTrue(ci_results.result_errors(needs))


if __name__ == "__main__":
    unittest.main()
