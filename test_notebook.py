##$$ 1
import duckdb as db
import pandas as pd
import matplotlib.pyplot as plt

##$$ 2
pd.set_option('display.max_columns', None)
pd.set_option('display.width', 10000)

##$$ 3
df  = pd.read_csv("/home/thyme/code/customers.csv")
print(df.head(5))

##$$ 4
df1 = pd.read_csv("/home/thyme/code/orders.csv")
print(df1.head(5))

##$$ 5
result = db.sql("""
    SELECT
    *
    FROM df a
    left join df1 b on a.customer_id = b.customer_id
    where a.customer_id = 3
""").fetchdf()
    
print(result.head(50))


##$$

# Simple scatter plot: quantity vs amount
plt.figure(figsize=(8, 6))
plt.scatter(result['quantity'], result['amount'])
plt.xlabel('Quantity')
plt.ylabel('Amount ($)')
plt.title('Order Quantity vs Amount')
plt.grid(True, alpha=0.3)

# Save and show
plt.savefig('scatter.png', dpi=100, bbox_inches='tight')
plt.show()  # This will open in your system's default image viewer
