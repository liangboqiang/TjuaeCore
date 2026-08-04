# Mermaid 语法速查

## 流程图

```mermaid
graph TD
    A[开始] --> B{判断}
    B -->|是| C[操作 1]
    B -->|否| D[操作 2]
    C --> E[结束]
    D --> E
```

方向：`TD`（从上到下）、`LR`（从左到右）、`BT`、`RL`。

节点形状：

- `A[text]`：矩形；
- `A(text)`：圆角矩形；
- `A{text}`：菱形；
- `A([text])`：胶囊形；
- `A[[text]]`：子程序；
- `A[(text)]`：圆柱形；
- `A((text))`：圆形。

## 时序图

```mermaid
sequenceDiagram
    participant A as 甲
    participant B as 乙
    A->>B: 你好
    B-->>A: 收到
    A->>+B: 请求
    B->>-A: 响应
    Note over A,B: 共享说明
```

箭头：`->>` 为实线、`-->>` 为虚线、`-x` 为终止、`-)` 为开放箭头。

## 状态图

```mermaid
stateDiagram-v2
    [*] --> 空闲
    空闲 --> 运行中: 启动
    运行中 --> 空闲: 停止
    运行中 --> [*]: 终止
```

## 类图

```mermaid
classDiagram
    class Animal {
        +String name
        +eat()
    }
    class Dog {
        +bark()
    }
    Animal <|-- Dog
```

关系：`<|--` 为继承、`*--` 为组合、`o--` 为聚合、`-->` 为关联。

## ER 图

```mermaid
erDiagram
    USER ||--o{ ORDER : places
    ORDER ||--|{ LINE-ITEM : contains
    PRODUCT ||--o{ LINE-ITEM : includes
```

基数：`||` 表示一个、`o|` 表示零个或一个、`}|` 表示一个或多个、`}o`
表示零个或多个。
