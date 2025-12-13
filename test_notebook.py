#!/home/thyme/venv/bin/python

##$$
#source /home/thyme/venv/bin/activate && pip install matplotlib

##$$ 1 ---------------------------------------------------------------------------
 
import duckdb as db
import pandas as pd
import matplotlib.pyplot as plt
import math
import numpy as np

##$$ 2 ---------------------------------------------------------------------------
 
pd.set_option('display.max_columns', None)
pd.set_option('display.width', 10000)

##$$ 3 ---------------------------------------------------------------------------
 
customers_file = "/home/thyme/code/customers.csv"
orders_file    = "/home/thyme/code/orders.csv"

##$$ 4 ---------------------------------------------------------------------------
 
print(db.sql(f"""select * from '{customers_file}' limit 3 """).fetchdf())
print("")
print(db.sql(f"""select * from '{orders_file}'    limit 3 """).fetchdf())

##$$ 5 ---------------------------------------------------------------------------
 
df = db.sql(f"""
    select
        *,
        sum(amount) over (partition by 1 order by signup_date rows between 15 preceding and current row) as rolling_sum_amount
    from '{customers_file}' a
    left join '{orders_file}' b on a.customer_id = b.customer_id
    where quantity is not null
    and amount is not null
    """).fetchdf()
print(df.head(3))

##$$ 6 ---------------------------------------------------------------------------
 
# plt.figure(figsize=(8, 6))
plt.scatter(df['signup_date'], df['rolling_sum_amount'])
# plt.xlabel('Quantity')
# plt.ylabel('Amount ($)')
# plt.title('Order Quantity vs Amount')
# plt.grid(True, alpha=0.3)
# plt.savefig('scatter.png', dpi=100, bbox_inches='tight')
plt.show()

##$$ 7 ---------------------------------------------------------------------------
 
for idx, row in df.iterrows():
    if row['amount'] > 100:
        print(row['name'], row['amount'])

##$$ 8 ---------------------------------------------------------------------------
 
for i in range(1000):
    print(np.random.randn(1))
