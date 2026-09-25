# Decision Firewall

A deterministic firewall to guarantee safety. The pipeline proceeds as:
Candidate Generation -> Prediction -> Counterfactual Evaluation -> Utility -> Decision Firewall

If any mandatory safety condition (such as state freshness, capacity, SLO constraints) fails, the request is safely rejected.
